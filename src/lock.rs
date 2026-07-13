use fs2::FileExt;
use std::fs::{File, OpenOptions};
use std::path::Path;

/// An advisory, process-lifetime lock for one persisted engine.
pub(crate) struct DataDirLock {
    file: File,
}

impl DataDirLock {
    pub(crate) fn acquire(data_dir: &Path, engine: &str) -> Result<Self, String> {
        std::fs::create_dir_all(data_dir)
            .map_err(|error| format!("Failed to create data directory: {error}"))?;
        let path = data_dir.join(format!(".{engine}.lock"));
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|error| format!("Failed to open {} lock: {error}", path.display()))?;
        file.try_lock_exclusive().map_err(|_| {
            format!(
                "The {engine} engine is active in data directory '{}'",
                data_dir.display()
            )
        })?;
        Ok(Self { file })
    }
}

impl Drop for DataDirLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}
