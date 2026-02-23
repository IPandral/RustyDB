use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// Operations that can be persisted to disk
#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum Operation {
    Set { key: String, value: String },
    Delete { key: String },
    Clear,
}

/// Configuration for persistence behavior
#[derive(Clone)]
pub struct PersistenceConfig {
    /// Directory where data files are stored
    pub data_dir: PathBuf,
    /// How often to flush WAL to disk (in milliseconds)
    pub flush_interval_ms: u64,
    /// WAL size threshold for automatic snapshot (in bytes)
    #[allow(dead_code)]
    pub snapshot_threshold: u64,
}

impl Default for PersistenceConfig {
    fn default() -> Self {
        Self {
            data_dir: PathBuf::from("./data"),
            flush_interval_ms: 1000, // 1 second
            snapshot_threshold: 10 * 1024 * 1024, // 10MB
        }
    }
}

impl PersistenceConfig {
    pub fn new(data_dir: impl AsRef<Path>) -> Self {
        Self {
            data_dir: data_dir.as_ref().to_path_buf(),
            ..Default::default()
        }
    }

    #[allow(dead_code)]
    pub fn with_flush_interval(mut self, ms: u64) -> Self {
        self.flush_interval_ms = ms;
        self
    }

    #[allow(dead_code)]
    pub fn with_snapshot_threshold(mut self, bytes: u64) -> Self {
        self.snapshot_threshold = bytes;
        self
    }
}

/// Manages persistence for KVStore with WAL and background flushing
pub struct PersistenceManager {
    config: PersistenceConfig,
    wal_path: PathBuf,
    snapshot_path: PathBuf,
    wal_file: Arc<Mutex<Option<File>>>,
    pending_ops: Arc<Mutex<Vec<Operation>>>,
    shutdown: Arc<AtomicBool>,
    flush_thread: Option<thread::JoinHandle<()>>,
}

impl PersistenceManager {
    /// Creates a new persistence manager with the given configuration
    pub fn new(config: PersistenceConfig) -> Result<Self, String> {
        std::fs::create_dir_all(&config.data_dir)
            .map_err(|e| format!("Failed to create directory: {}", e))?;

        let wal_path = config.data_dir.join("rustydb.wal");
        let snapshot_path = config.data_dir.join("rustydb.db");

        // Open WAL file in append mode
        let wal_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&wal_path)
            .map_err(|e| format!("Failed to open WAL: {}", e))?;

        let mut manager = Self {
            config,
            wal_path,
            snapshot_path,
            wal_file: Arc::new(Mutex::new(Some(wal_file))),
            pending_ops: Arc::new(Mutex::new(Vec::new())),
            shutdown: Arc::new(AtomicBool::new(false)),
            flush_thread: None,
        };

        manager.start_background_flush();

        Ok(manager)
    }

    /// Starts the background flush thread
    fn start_background_flush(&mut self) {
        let pending_ops = Arc::clone(&self.pending_ops);
        let wal_file = Arc::clone(&self.wal_file);
        let shutdown = Arc::clone(&self.shutdown);
        let flush_interval = Duration::from_millis(self.config.flush_interval_ms);

        let handle = thread::spawn(move || {
            while !shutdown.load(Ordering::Relaxed) {
                thread::sleep(flush_interval);

                // Flush pending operations
                let ops_to_flush: Vec<Operation> = {
                    let mut pending = pending_ops.lock().unwrap();
                    std::mem::take(&mut *pending)
                };

                if !ops_to_flush.is_empty() {
                    if let Ok(mut file_guard) = wal_file.lock() {
                        if let Some(ref mut file) = *file_guard {
                            for op in &ops_to_flush {
                                if let Ok(json) = serde_json::to_string(op) {
                                    let _ = writeln!(file, "{}", json);
                                }
                            }
                            let _ = file.sync_all();
                        }
                    }
                }
            }
        });

        self.flush_thread = Some(handle);
    }

    /// Queues an operation for background flushing (non-blocking)
    pub fn log_operation_async(&self, op: Operation) {
        let mut pending = self.pending_ops.lock().unwrap();
        pending.push(op);
    }

    /// Logs an operation immediately to WAL (blocking, crash-safe)
    pub fn log_operation_sync(&self, op: &Operation) -> Result<(), String> {
        let json = serde_json::to_string(op)
            .map_err(|e| format!("Failed to serialize operation: {}", e))?;

        let mut file_guard = self.wal_file.lock().unwrap();
        if let Some(ref mut file) = *file_guard {
            writeln!(file, "{}", json)
                .map_err(|e| format!("Failed to write to WAL: {}", e))?;
            file.sync_all()
                .map_err(|e| format!("Failed to sync WAL: {}", e))?;
        }
        Ok(())
    }

    /// Recovers data from snapshot and WAL
    pub fn recover(&self) -> Result<Vec<Operation>, String> {
        let mut operations = Vec::new();

        // First, load snapshot if it exists
        if self.snapshot_path.exists() {
            let file = File::open(&self.snapshot_path)
                .map_err(|e| format!("Failed to open snapshot: {}", e))?;
            let reader = BufReader::new(file);

            for line in reader.lines() {
                let line = line.map_err(|e| format!("Failed to read line: {}", e))?;
                if line.trim().is_empty() {
                    continue;
                }
                let op: Operation = serde_json::from_str(&line)
                    .map_err(|e| format!("Failed to parse operation: {}", e))?;
                operations.push(op);
            }
        }

        // Then, replay WAL on top of snapshot
        if self.wal_path.exists() {
            let file = File::open(&self.wal_path)
                .map_err(|e| format!("Failed to open WAL: {}", e))?;
            let reader = BufReader::new(file);

            for line in reader.lines() {
                let line = line.map_err(|e| format!("Failed to read line: {}", e))?;
                if line.trim().is_empty() {
                    continue;
                }
                // Skip corrupted lines (partial writes from crashes)
                if let Ok(op) = serde_json::from_str::<Operation>(&line) {
                    operations.push(op);
                }
            }
        }

        Ok(operations)
    }

    /// Creates a snapshot from current data (compacts the WAL)
    pub fn create_snapshot(&self, data: &[(String, String)]) -> Result<(), String> {
        // Write to temporary file first (atomic rename)
        let temp_path = self.snapshot_path.with_extension("tmp");

        let mut file = File::create(&temp_path)
            .map_err(|e| format!("Failed to create snapshot: {}", e))?;

        // Write only Set operations for current state
        for (key, value) in data {
            let op = Operation::Set {
                key: key.clone(),
                value: value.clone(),
            };
            let json = serde_json::to_string(&op)
                .map_err(|e| format!("Failed to serialize operation: {}", e))?;
            writeln!(file, "{}", json)
                .map_err(|e| format!("Failed to write snapshot: {}", e))?;
        }

        file.sync_all()
            .map_err(|e| format!("Failed to sync snapshot: {}", e))?;

        // Atomic rename
        std::fs::rename(&temp_path, &self.snapshot_path)
            .map_err(|e| format!("Failed to rename snapshot: {}", e))?;

        // Clear WAL after successful snapshot
        {
            let mut file_guard = self.wal_file.lock().unwrap();
            *file_guard = None;
        }
        std::fs::remove_file(&self.wal_path).ok();

        // Reopen WAL
        let new_wal = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.wal_path)
            .map_err(|e| format!("Failed to reopen WAL: {}", e))?;

        {
            let mut file_guard = self.wal_file.lock().unwrap();
            *file_guard = Some(new_wal);
        }

        Ok(())
    }

    /// Returns WAL size in bytes
    pub fn wal_size(&self) -> Result<u64, String> {
        if self.wal_path.exists() {
            std::fs::metadata(&self.wal_path)
                .map(|m| m.len())
                .map_err(|e| format!("Failed to get WAL size: {}", e))
        } else {
            Ok(0)
        }
    }

    /// Flushes all pending operations immediately
    pub fn flush(&self) -> Result<(), String> {
        let ops_to_flush: Vec<Operation> = {
            let mut pending = self.pending_ops.lock().unwrap();
            std::mem::take(&mut *pending)
        };

        for op in &ops_to_flush {
            self.log_operation_sync(op)?;
        }

        Ok(())
    }

    /// Shuts down the persistence manager gracefully
    #[allow(dead_code)]
    pub fn shutdown(&mut self) -> Result<(), String> {
        self.shutdown.store(true, Ordering::Relaxed);

        self.flush()?;

        if let Some(handle) = self.flush_thread.take() {
            handle.join().map_err(|_| "Failed to join flush thread")?;
        }

        Ok(())
    }
}

impl Drop for PersistenceManager {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        let _ = self.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_log_and_recover() {
        let temp_dir = TempDir::new().unwrap();
        let config = PersistenceConfig::new(temp_dir.path());
        let pm = PersistenceManager::new(config).unwrap();

        // Log some operations synchronously
        pm.log_operation_sync(&Operation::Set {
            key: "key1".to_string(),
            value: "value1".to_string(),
        })
        .unwrap();

        pm.log_operation_sync(&Operation::Set {
            key: "key2".to_string(),
            value: "value2".to_string(),
        })
        .unwrap();

        // Recover
        let ops = pm.recover().unwrap();
        assert_eq!(ops.len(), 2);
    }

    #[test]
    fn test_snapshot() {
        let temp_dir = TempDir::new().unwrap();
        let config = PersistenceConfig::new(temp_dir.path());
        let pm = PersistenceManager::new(config).unwrap();

        let data = vec![
            ("key1".to_string(), "value1".to_string()),
            ("key2".to_string(), "value2".to_string()),
        ];

        pm.create_snapshot(&data).unwrap();

        // Recover from snapshot
        let recovered = pm.recover().unwrap();
        assert_eq!(recovered.len(), 2);
    }

    #[test]
    fn test_async_flush() {
        let temp_dir = TempDir::new().unwrap();
        let config = PersistenceConfig::new(temp_dir.path()).with_flush_interval(100);
        let pm = PersistenceManager::new(config).unwrap();

        // Queue async operations
        pm.log_operation_async(Operation::Set {
            key: "async_key".to_string(),
            value: "async_value".to_string(),
        });

        // Wait for flush
        thread::sleep(Duration::from_millis(200));

        // Recover should see the operation
        let ops = pm.recover().unwrap();
        assert_eq!(ops.len(), 1);
    }

    #[test]
    fn test_configurable_flush_interval() {
        let temp_dir = TempDir::new().unwrap();
        let config = PersistenceConfig::new(temp_dir.path())
            .with_flush_interval(50)
            .with_snapshot_threshold(1024);

        assert_eq!(config.flush_interval_ms, 50);
        assert_eq!(config.snapshot_threshold, 1024);
    }
}
