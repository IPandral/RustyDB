use crc32fast::Hasher;
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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

const KV_FORMAT_VERSION: u32 = 2;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub(crate) struct KvWalRecord {
    pub(crate) format_version: u32,
    pub(crate) sequence: u64,
    pub(crate) timestamp_millis: u64,
    pub(crate) operation: Operation,
    pub(crate) checksum: u32,
}

impl KvWalRecord {
    fn new(sequence: u64, operation: Operation) -> Self {
        let timestamp_millis = now_millis();
        let checksum = kv_checksum(sequence, timestamp_millis, &operation);
        Self {
            format_version: KV_FORMAT_VERSION,
            sequence,
            timestamp_millis,
            operation,
            checksum,
        }
    }

    pub(crate) fn valid(&self) -> bool {
        self.format_version == KV_FORMAT_VERSION
            && self.checksum == kv_checksum(self.sequence, self.timestamp_millis, &self.operation)
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct KvSnapshot {
    pub(crate) format_version: u32,
    pub(crate) sequence: u64,
    #[serde(default)]
    pub(crate) timestamp_millis: u64,
    pub(crate) entries: Vec<(String, String)>,
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn kv_checksum(sequence: u64, timestamp_millis: u64, operation: &Operation) -> u32 {
    let mut hasher = Hasher::new();
    hasher.update(&sequence.to_le_bytes());
    hasher.update(&timestamp_millis.to_le_bytes());
    hasher.update(&serde_json::to_vec(operation).unwrap_or_default());
    hasher.finalize()
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
            flush_interval_ms: 1000,              // 1 second
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
    next_sequence: Arc<AtomicU64>,
    _data_lock: crate::lock::DataDirLock,
    flush_thread: Option<thread::JoinHandle<()>>,
}

impl PersistenceManager {
    /// Creates a new persistence manager with the given configuration
    pub fn new(config: PersistenceConfig) -> Result<Self, String> {
        std::fs::create_dir_all(&config.data_dir)
            .map_err(|e| format!("Failed to create directory: {}", e))?;

        let data_lock = crate::lock::DataDirLock::acquire(&config.data_dir, "kv")?;
        let wal_path = config.data_dir.join("rustydb.wal");
        let snapshot_path = config.data_dir.join("rustydb.db");
        let next_sequence = scan_max_sequence(&snapshot_path, &wal_path).saturating_add(1);

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
            next_sequence: Arc::new(AtomicU64::new(next_sequence)),
            _data_lock: data_lock,
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
        let next_sequence = Arc::clone(&self.next_sequence);
        let flush_interval = Duration::from_millis(self.config.flush_interval_ms);

        let handle = thread::spawn(move || {
            while !shutdown.load(Ordering::Relaxed) {
                thread::sleep(flush_interval);

                // Flush pending operations
                let ops_to_flush: Vec<Operation> = {
                    let mut pending = pending_ops.lock().unwrap();
                    std::mem::take(&mut *pending)
                };

                if !ops_to_flush.is_empty()
                    && let Ok(mut file_guard) = wal_file.lock()
                    && let Some(ref mut file) = *file_guard
                {
                    for op in ops_to_flush {
                        let record =
                            KvWalRecord::new(next_sequence.fetch_add(1, Ordering::SeqCst), op);
                        if let Ok(json) = serde_json::to_string(&record) {
                            let _ = writeln!(file, "{}", json);
                        }
                    }
                    let _ = file.sync_all();
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
        let mut file_guard = self.wal_file.lock().unwrap();
        if let Some(ref mut file) = *file_guard {
            let record = KvWalRecord::new(
                self.next_sequence.fetch_add(1, Ordering::SeqCst),
                op.clone(),
            );
            let json = serde_json::to_string(&record)
                .map_err(|e| format!("Failed to serialize operation: {}", e))?;
            writeln!(file, "{}", json).map_err(|e| format!("Failed to write to WAL: {}", e))?;
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
            let content = std::fs::read_to_string(&self.snapshot_path)
                .map_err(|e| format!("Failed to open snapshot: {}", e))?;
            if let Ok(snapshot) = serde_json::from_str::<KvSnapshot>(&content) {
                operations.extend(
                    snapshot
                        .entries
                        .into_iter()
                        .map(|(key, value)| Operation::Set { key, value }),
                );
            } else {
                for line in content.lines() {
                    if line.trim().is_empty() {
                        continue;
                    }
                    let op: Operation = serde_json::from_str(line)
                        .map_err(|e| format!("Failed to parse operation: {}", e))?;
                    operations.push(op);
                }
            }
        }

        // Then, replay WAL on top of snapshot
        if self.wal_path.exists() {
            let file =
                File::open(&self.wal_path).map_err(|e| format!("Failed to open WAL: {}", e))?;
            let reader = BufReader::new(file);

            for line in reader.lines() {
                let line = line.map_err(|e| format!("Failed to read line: {}", e))?;
                if line.trim().is_empty() {
                    continue;
                }
                // Skip corrupted lines (partial writes from crashes)
                if let Ok(record) = serde_json::from_str::<KvWalRecord>(&line) {
                    if !record.valid() {
                        break;
                    }
                    operations.push(record.operation);
                } else if let Ok(op) = serde_json::from_str::<Operation>(&line) {
                    operations.push(op);
                }
            }
        }

        Ok(operations)
    }

    /// Creates a snapshot from current data (compacts the WAL)
    pub fn create_snapshot(&self, data: &[(String, String)]) -> Result<(), String> {
        self.flush()?;
        // Write to temporary file first (atomic rename)
        let temp_path = self.snapshot_path.with_extension("tmp");

        let mut file =
            File::create(&temp_path).map_err(|e| format!("Failed to create snapshot: {}", e))?;

        let snapshot = KvSnapshot {
            format_version: KV_FORMAT_VERSION,
            sequence: self.current_sequence(),
            timestamp_millis: now_millis(),
            entries: data.to_vec(),
        };
        let json = serde_json::to_vec_pretty(&snapshot)
            .map_err(|e| format!("Failed to serialize snapshot: {}", e))?;
        file.write_all(&json)
            .map_err(|e| format!("Failed to write snapshot: {}", e))?;

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
        if std::fs::metadata(&self.wal_path).is_ok_and(|metadata| metadata.len() > 0) {
            let archive = self.config.data_dir.join("wal_archive").join("kv");
            std::fs::create_dir_all(&archive)
                .map_err(|e| format!("Failed to create KV WAL archive: {e}"))?;
            std::fs::rename(
                &self.wal_path,
                archive.join(format!(
                    "kv-{}-{}.wal",
                    self.current_sequence(),
                    now_millis()
                )),
            )
            .map_err(|e| format!("Failed to archive KV WAL: {e}"))?;
        }

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

    pub fn current_sequence(&self) -> u64 {
        self.next_sequence.load(Ordering::SeqCst).saturating_sub(1)
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

fn scan_max_sequence(snapshot_path: &Path, wal_path: &Path) -> u64 {
    let snapshot_sequence = std::fs::read_to_string(snapshot_path)
        .ok()
        .and_then(|content| serde_json::from_str::<KvSnapshot>(&content).ok())
        .map(|snapshot| snapshot.sequence)
        .unwrap_or(0);
    let wal_sequence = File::open(wal_path)
        .ok()
        .map(|file| {
            BufReader::new(file)
                .lines()
                .map_while(Result::ok)
                .filter_map(|line| serde_json::from_str::<KvWalRecord>(&line).ok())
                .filter(KvWalRecord::valid)
                .map(|record| record.sequence)
                .max()
                .unwrap_or(0)
        })
        .unwrap_or(0);
    snapshot_sequence.max(wal_sequence)
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
