use super::persistence::{Operation, PersistenceConfig, PersistenceManager};
use dashmap::DashMap;
use std::sync::Arc;
use std::sync::Mutex;

/// High-performance concurrent key-value store using DashMap with Arc<String> values for zero-copy reads
#[derive(Clone)]
pub struct KVStore {
    data: Arc<DashMap<String, Arc<String>>>,
    persistence: Arc<Mutex<Option<PersistenceManager>>>,
}

impl KVStore {
    /// Creates a new empty KVStore (in-memory only, no persistence)
    pub fn new() -> Self {
        KVStore {
            data: Arc::new(DashMap::new()),
            persistence: Arc::new(Mutex::new(None)),
        }
    }

    /// Creates a new KVStore with persistence enabled
    pub fn with_persistence(config: PersistenceConfig) -> Result<Self, String> {
        let pm = PersistenceManager::new(config)?;
        let store = KVStore {
            data: Arc::new(DashMap::new()),
            persistence: Arc::new(Mutex::new(Some(pm))),
        };

        // Recover from disk
        store.recover()?;
        Ok(store)
    }

    /// Creates a KVStore with persistence at the given directory (convenience method)
    pub fn open(data_dir: &str) -> Result<Self, String> {
        Self::with_persistence(PersistenceConfig::new(data_dir))
    }

    /// Recovers data from disk
    fn recover(&self) -> Result<(), String> {
        let ops = {
            let guard = self
                .persistence
                .lock()
                .map_err(|e| format!("Lock error: {}", e))?;
            if let Some(ref pm) = *guard {
                pm.recover()?
            } else {
                return Ok(());
            }
        };

        for op in ops {
            match op {
                Operation::Set { key, value } => {
                    self.data.insert(key, Arc::new(value));
                }
                Operation::Delete { key } => {
                    self.data.remove(&key);
                }
                Operation::Clear => {
                    self.data.clear();
                }
            }
        }

        Ok(())
    }

    /// Inserts a key-value pair
    pub fn set(&self, key: String, value: String) -> Result<(), String> {
        // Write to WAL first (crash-safe)
        if let Ok(mut guard) = self.persistence.lock()
            && let Some(ref mut pm) = *guard
        {
            pm.log_operation_sync(&Operation::Set {
                key: key.clone(),
                value: value.clone(),
            })?;
        }

        // Then update in-memory
        self.data.insert(key, Arc::new(value));
        Ok(())
    }

    /// Inserts a key-value pair asynchronously (faster, but less crash-safe)
    pub fn set_async(&self, key: String, value: String) -> Result<(), String> {
        // Queue for background flush
        if let Ok(guard) = self.persistence.lock()
            && let Some(ref pm) = *guard
        {
            pm.log_operation_async(Operation::Set {
                key: key.clone(),
                value: value.clone(),
            });
        }

        // Update in-memory immediately
        self.data.insert(key, Arc::new(value));
        Ok(())
    }

    /// Returns Arc<String> for zero-copy reads
    pub fn get(&self, key: &str) -> Result<Option<Arc<String>>, String> {
        Ok(self.data.get(key).map(|guard| Arc::clone(guard.value())))
    }

    /// Retrieves multiple keys at once
    pub fn get_many(&self, keys: &[&str]) -> Result<Vec<(String, Arc<String>)>, String> {
        Ok(keys
            .iter()
            .filter_map(|&key| {
                self.data
                    .get(key)
                    .map(|guard| (key.to_string(), Arc::clone(guard.value())))
            })
            .collect())
    }

    /// Inserts multiple key-value pairs, returns count
    pub fn set_many(&self, pairs: Vec<(String, String)>) -> Result<usize, String> {
        let count = pairs.len();
        for (key, value) in pairs {
            self.set(key, value)?;
        }
        Ok(count)
    }

    /// Removes a key, returns true if it existed
    pub fn delete(&self, key: &str) -> Result<bool, String> {
        // Write to WAL first
        if let Ok(mut guard) = self.persistence.lock()
            && let Some(ref mut pm) = *guard
        {
            pm.log_operation_sync(&Operation::Delete {
                key: key.to_string(),
            })?;
        }

        Ok(self.data.remove(key).is_some())
    }

    /// Returns the number of keys
    pub fn len(&self) -> Result<usize, String> {
        Ok(self.data.len())
    }

    /// Returns true if the store is empty
    pub fn is_empty(&self) -> Result<bool, String> {
        Ok(self.data.is_empty())
    }

    /// Removes all keys
    pub fn clear(&self) -> Result<(), String> {
        // Write to WAL first
        if let Ok(mut guard) = self.persistence.lock()
            && let Some(ref mut pm) = *guard
        {
            pm.log_operation_sync(&Operation::Clear)?;
        }

        self.data.clear();
        Ok(())
    }

    /// Creates a snapshot of current data (compacts the WAL)
    pub fn snapshot(&self) -> Result<(), String> {
        let guard = self
            .persistence
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;
        if let Some(ref pm) = *guard {
            let data: Vec<(String, String)> = self
                .data
                .iter()
                .map(|entry| (entry.key().clone(), entry.value().to_string()))
                .collect();
            pm.create_snapshot(&data)?;
        }
        Ok(())
    }

    /// Returns current WAL size in bytes
    pub fn wal_size(&self) -> Result<u64, String> {
        let guard = self
            .persistence
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;
        if let Some(ref pm) = *guard {
            pm.wal_size()
        } else {
            Ok(0)
        }
    }

    /// Flushes any pending async writes to disk
    pub fn flush(&self) -> Result<(), String> {
        let guard = self
            .persistence
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;
        if let Some(ref pm) = *guard {
            pm.flush()?;
        }
        Ok(())
    }
}

impl Default for KVStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_set_and_get() {
        let store = KVStore::new();

        store
            .set("name".to_string(), "RustyDB".to_string())
            .unwrap();

        let value = store.get("name").unwrap();
        assert_eq!(value.as_ref().map(|s| s.as_str()), Some("RustyDB"));
    }

    #[test]
    fn test_get_nonexistent() {
        let store = KVStore::new();

        let value = store.get("nonexistent").unwrap();
        assert_eq!(value, None);
    }

    #[test]
    fn test_batch_operations() {
        let store = KVStore::new();

        // Batch set
        let pairs = vec![
            ("key1".to_string(), "value1".to_string()),
            ("key2".to_string(), "value2".to_string()),
            ("key3".to_string(), "value3".to_string()),
        ];
        let count = store.set_many(pairs).unwrap();
        assert_eq!(count, 3);

        // Batch get
        let keys = ["key1", "key2", "key3", "nonexistent"];
        let results = store.get_many(&keys).unwrap();
        assert_eq!(results.len(), 3);

        // Verify values
        for (key, value) in results {
            assert!(key.starts_with("key"));
            assert!(value.starts_with("value"));
        }
    }

    #[test]
    fn test_delete() {
        let store = KVStore::new();

        store.set("temp".to_string(), "value".to_string()).unwrap();
        assert_eq!(store.len().unwrap(), 1);

        let deleted = store.delete("temp").unwrap();
        assert!(deleted);
        assert_eq!(store.len().unwrap(), 0);

        // Try deleting again
        let deleted = store.delete("temp").unwrap();
        assert!(!deleted);
    }

    #[test]
    fn test_clear() {
        let store = KVStore::new();

        store.set("key1".to_string(), "value1".to_string()).unwrap();
        store.set("key2".to_string(), "value2".to_string()).unwrap();
        assert_eq!(store.len().unwrap(), 2);

        store.clear().unwrap();
        assert_eq!(store.len().unwrap(), 0);
    }

    #[test]
    fn test_concurrent_reads() {
        use std::thread;

        let store = KVStore::new();
        store
            .set("shared".to_string(), "value".to_string())
            .unwrap();

        let mut handles = vec![];

        for _ in 0..10 {
            let store_clone = store.clone();
            let handle = thread::spawn(move || {
                for _ in 0..100 {
                    let value = store_clone.get("shared").unwrap();
                    assert_eq!(value.as_ref().map(|s| s.as_str()), Some("value"));
                }
            });
            handles.push(handle);
        }

        for handle in handles {
            handle.join().unwrap();
        }
    }

    #[test]
    fn test_persistence() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().to_str().unwrap();

        // Create store and add data
        {
            let store = KVStore::open(path).unwrap();
            store
                .set("persistent_key".to_string(), "persistent_value".to_string())
                .unwrap();
            store.snapshot().unwrap();
        }

        // Reopen and verify data persisted
        {
            let store = KVStore::open(path).unwrap();
            let value = store.get("persistent_key").unwrap();
            assert_eq!(value.as_ref().map(|s| s.as_str()), Some("persistent_value"));
        }
    }

    #[test]
    fn test_crash_recovery() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().to_str().unwrap();

        // Write to WAL without snapshot (simulating crash)
        {
            let store = KVStore::open(path).unwrap();
            store
                .set("wal_key".to_string(), "wal_value".to_string())
                .unwrap();
            // No snapshot - data only in WAL
        }

        // Recover from WAL
        {
            let store = KVStore::open(path).unwrap();
            let value = store.get("wal_key").unwrap();
            assert_eq!(value.as_ref().map(|s| s.as_str()), Some("wal_value"));
        }
    }

    #[test]
    fn test_async_writes() {
        let temp_dir = TempDir::new().unwrap();
        let config = PersistenceConfig::new(temp_dir.path()).with_flush_interval(50);
        let store = KVStore::with_persistence(config).unwrap();

        // Async write
        store
            .set_async("async_key".to_string(), "async_value".to_string())
            .unwrap();

        // Data is immediately available in memory
        let value = store.get("async_key").unwrap();
        assert_eq!(value.as_ref().map(|s| s.as_str()), Some("async_value"));

        // Flush to ensure it's on disk
        store.flush().unwrap();
    }
}
