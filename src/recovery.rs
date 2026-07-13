use crc32fast::Hasher;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::persistence::{KvSnapshot, KvWalRecord};
use crate::sql::database::{CatalogSnapshot, WalRecord};

const BACKUP_FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryPoint {
    pub sql_commit_version: u64,
    pub kv_sequence: u64,
}

impl FromStr for RecoveryPoint {
    type Err = String;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let mut sql = None;
        let mut kv = None;
        for part in value.split(',') {
            let (name, value) = part
                .split_once(':')
                .ok_or_else(|| "Recovery point must be sql:N,kv:N".to_string())?;
            match name.trim().to_ascii_lowercase().as_str() {
                "sql" => {
                    sql = Some(
                        value
                            .trim()
                            .parse()
                            .map_err(|_| "Invalid SQL commit version".to_string())?,
                    )
                }
                "kv" => {
                    kv = Some(
                        value
                            .trim()
                            .parse()
                            .map_err(|_| "Invalid KV sequence".to_string())?,
                    )
                }
                _ => return Err("Recovery point must be sql:N,kv:N".to_string()),
            }
        }
        Ok(Self {
            sql_commit_version: sql.ok_or_else(|| "Recovery point is missing sql:N".to_string())?,
            kv_sequence: kv.ok_or_else(|| "Recovery point is missing kv:N".to_string())?,
        })
    }
}

#[derive(Debug, Clone)]
pub enum RecoveryTarget {
    Latest,
    Timestamp(SystemTime),
    Point(RecoveryPoint),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupFile {
    pub path: String,
    pub size: u64,
    pub checksum: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupManifest {
    pub format_version: u32,
    pub created_at_millis: u64,
    pub base_point: RecoveryPoint,
    pub recovery_point: RecoveryPoint,
    pub sql_base_timestamp_millis: u64,
    pub kv_base_timestamp_millis: u64,
    pub files: Vec<BackupFile>,
}

#[derive(Debug, Clone, Default)]
pub struct PruneReport {
    pub files: Vec<PathBuf>,
    pub bytes: u64,
    pub applied: bool,
}

pub struct BackupManager;

impl BackupManager {
    /// Create a consistent offline backup. Fails if either persisted engine is active.
    pub fn create(
        source: impl AsRef<Path>,
        destination: impl AsRef<Path>,
    ) -> Result<BackupManifest, String> {
        let source = source.as_ref();
        let destination = destination.as_ref();
        ensure_destination_empty(destination)?;
        let _sql_lock = crate::lock::DataDirLock::acquire(source, "sql")?;
        let _kv_lock = crate::lock::DataDirLock::acquire(source, "kv")?;
        fs::create_dir_all(destination)
            .map_err(|error| format!("Failed to create backup directory: {error}"))?;

        let base = destination.join("base");
        fs::create_dir_all(&base)
            .map_err(|error| format!("Failed to create backup base: {error}"))?;
        for name in ["catalog.json", "sql_wal.log", "rustydb.db", "rustydb.wal"] {
            let path = source.join(name);
            if path.exists() {
                copy_file(&path, &base.join(name))?;
            }
        }
        let source_archive = source.join("wal_archive");
        if source_archive.exists() {
            copy_tree(&source_archive, &destination.join("wal_archive"))?;
        }

        let (sql_base, sql_base_timestamp) = read_sql_snapshot(&base.join("catalog.json"))?;
        let (kv_base, kv_base_timestamp) = read_kv_snapshot(&base.join("rustydb.db"))?;
        let sql_end = max_sql_version(destination, sql_base);
        let kv_end = max_kv_sequence(destination, kv_base);
        let mut files = list_files(destination)?;
        files.retain(|path| {
            path.file_name()
                .is_none_or(|name| name != "manifest.json" && name != "manifest.json.tmp")
        });
        let files = files
            .into_iter()
            .map(|path| {
                let relative = path
                    .strip_prefix(destination)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                Ok(BackupFile {
                    path: relative,
                    size: fs::metadata(&path).map_err(|e| e.to_string())?.len(),
                    checksum: checksum_file(&path)?,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let manifest = BackupManifest {
            format_version: BACKUP_FORMAT_VERSION,
            created_at_millis: now_millis(),
            base_point: RecoveryPoint {
                sql_commit_version: sql_base,
                kv_sequence: kv_base,
            },
            recovery_point: RecoveryPoint {
                sql_commit_version: sql_end,
                kv_sequence: kv_end,
            },
            sql_base_timestamp_millis: sql_base_timestamp,
            kv_base_timestamp_millis: kv_base_timestamp,
            files,
        };
        let temporary = destination.join("manifest.json.tmp");
        let final_path = destination.join("manifest.json");
        let encoded = serde_json::to_vec_pretty(&manifest)
            .map_err(|error| format!("Failed to serialize backup manifest: {error}"))?;
        let mut file = File::create(&temporary)
            .map_err(|error| format!("Failed to create backup manifest: {error}"))?;
        file.write_all(&encoded)
            .map_err(|error| format!("Failed to write backup manifest: {error}"))?;
        file.sync_all()
            .map_err(|error| format!("Failed to sync backup manifest: {error}"))?;
        fs::rename(temporary, final_path)
            .map_err(|error| format!("Failed to publish backup manifest: {error}"))?;
        Ok(manifest)
    }

    pub fn restore(
        backup: impl AsRef<Path>,
        wal_archive: Option<&Path>,
        destination: impl AsRef<Path>,
        target: RecoveryTarget,
    ) -> Result<RecoveryPoint, String> {
        let backup = backup.as_ref();
        let destination = destination.as_ref();
        ensure_destination_empty(destination)?;
        let manifest: BackupManifest = serde_json::from_slice(
            &fs::read(backup.join("manifest.json"))
                .map_err(|error| format!("Failed to read backup manifest: {error}"))?,
        )
        .map_err(|error| format!("Failed to parse backup manifest: {error}"))?;
        if manifest.format_version != BACKUP_FORMAT_VERSION {
            return Err(format!(
                "Unsupported backup format {}",
                manifest.format_version
            ));
        }
        validate_manifest(backup, &manifest)?;
        fs::create_dir_all(destination)
            .map_err(|error| format!("Failed to create restore directory: {error}"))?;
        let _sql_lock = crate::lock::DataDirLock::acquire(destination, "sql")?;
        let _kv_lock = crate::lock::DataDirLock::acquire(destination, "kv")?;

        for name in ["catalog.json", "rustydb.db"] {
            let source = backup.join("base").join(name);
            if source.exists() {
                copy_file(&source, &destination.join(name))?;
            }
        }
        if backup.join("wal_archive").exists() {
            copy_tree(
                &backup.join("wal_archive"),
                &destination.join("wal_archive"),
            )?;
        }

        let cutoff = match &target {
            RecoveryTarget::Timestamp(time) => Some(system_time_millis(*time)?),
            _ => None,
        };
        let latest_target = matches!(&target, RecoveryTarget::Latest);
        if let Some(cutoff) = cutoff {
            let base_time = manifest
                .sql_base_timestamp_millis
                .max(manifest.kv_base_timestamp_millis);
            if base_time != 0 && cutoff < base_time {
                return Err("Recovery timestamp predates the backup base snapshots".to_string());
            }
        }
        let requested_point = match target {
            RecoveryTarget::Point(point) => Some(point),
            _ => None,
        };
        if requested_point.is_some_and(|point| {
            point.sql_commit_version < manifest.base_point.sql_commit_version
                || point.kv_sequence < manifest.base_point.kv_sequence
        }) {
            return Err("Recovery point predates the backup base snapshots".to_string());
        }

        let mut roots = vec![backup.to_path_buf()];
        if let Some(archive) = wal_archive {
            roots.push(archive.to_path_buf());
        }
        let sql_records = collect_sql_records(&roots)?;
        let kv_records = collect_kv_records(&roots)?;
        let mut restored = manifest.base_point;
        let mut sql_file = File::create(destination.join("sql_wal.log"))
            .map_err(|error| format!("Failed to create restored SQL WAL: {error}"))?;
        let legacy_sql = legacy_lines(&backup.join("base").join("sql_wal.log"), true)?;
        if !legacy_sql.is_empty() && !latest_target {
            return Err("Timestamp/exact recovery requires the versioned SQL WAL format; restore Latest once to upgrade legacy WAL".to_string());
        }
        for line in legacy_sql {
            writeln!(sql_file, "{line}").map_err(|e| e.to_string())?;
        }
        for record in sql_records.values() {
            if record.commit_version <= manifest.base_point.sql_commit_version {
                continue;
            }
            if cutoff.is_some_and(|cutoff| record.timestamp_millis > cutoff) {
                break;
            }
            if requested_point.is_some_and(|point| record.commit_version > point.sql_commit_version)
            {
                continue;
            }
            if record.base_version != restored.sql_commit_version {
                return Err(format!(
                    "Missing SQL WAL segment between commit {} and {}",
                    restored.sql_commit_version, record.commit_version
                ));
            }
            writeln!(
                sql_file,
                "{}",
                serde_json::to_string(record).map_err(|e| e.to_string())?
            )
            .map_err(|e| e.to_string())?;
            restored.sql_commit_version = record.commit_version;
        }
        sql_file
            .sync_all()
            .map_err(|error| format!("Failed to sync restored SQL WAL: {error}"))?;
        let mut kv_file = File::create(destination.join("rustydb.wal"))
            .map_err(|error| format!("Failed to create restored KV WAL: {error}"))?;
        let legacy_kv = legacy_lines(&backup.join("base").join("rustydb.wal"), false)?;
        if !legacy_kv.is_empty() && !latest_target {
            return Err("Timestamp/exact recovery requires the versioned KV WAL format; restore Latest once to upgrade legacy WAL".to_string());
        }
        for line in legacy_kv {
            writeln!(kv_file, "{line}").map_err(|e| e.to_string())?;
        }
        for record in kv_records.values() {
            if record.sequence <= manifest.base_point.kv_sequence {
                continue;
            }
            if cutoff.is_some_and(|cutoff| record.timestamp_millis > cutoff) {
                break;
            }
            if requested_point.is_some_and(|point| record.sequence > point.kv_sequence) {
                continue;
            }
            if record.sequence != restored.kv_sequence.saturating_add(1) {
                return Err(format!(
                    "Missing KV WAL record after sequence {}",
                    restored.kv_sequence
                ));
            }
            writeln!(
                kv_file,
                "{}",
                serde_json::to_string(record).map_err(|e| e.to_string())?
            )
            .map_err(|e| e.to_string())?;
            restored.kv_sequence = record.sequence;
        }
        kv_file
            .sync_all()
            .map_err(|error| format!("Failed to sync restored KV WAL: {error}"))?;
        if let Some(point) = requested_point
            && restored != point
        {
            return Err(format!(
                "Requested recovery point sql:{},kv:{} is not fully available (restored sql:{},kv:{})",
                point.sql_commit_version,
                point.kv_sequence,
                restored.sql_commit_version,
                restored.kv_sequence
            ));
        }
        Ok(restored)
    }

    pub fn prune(
        data_dir: impl AsRef<Path>,
        before: SystemTime,
        apply: bool,
    ) -> Result<PruneReport, String> {
        let data_dir = data_dir.as_ref();
        let _sql_lock = crate::lock::DataDirLock::acquire(data_dir, "sql")?;
        let _kv_lock = crate::lock::DataDirLock::acquire(data_dir, "kv")?;
        let cutoff = system_time_millis(before)?;
        let archive = data_dir.join("wal_archive");
        let mut report = PruneReport {
            applied: apply,
            ..PruneReport::default()
        };
        for path in list_files(&archive)? {
            let latest = latest_record_timestamp(&path)?;
            if latest.is_some_and(|timestamp| timestamp < cutoff) {
                report.bytes += fs::metadata(&path).map_err(|e| e.to_string())?.len();
                report.files.push(path.clone());
                if apply {
                    fs::remove_file(&path)
                        .map_err(|error| format!("Failed to prune {}: {error}", path.display()))?;
                }
            }
        }
        Ok(report)
    }
}

fn validate_manifest(root: &Path, manifest: &BackupManifest) -> Result<(), String> {
    for entry in &manifest.files {
        let path = root.join(&entry.path);
        let metadata = fs::metadata(&path)
            .map_err(|error| format!("Backup file '{}' is missing: {error}", entry.path))?;
        if metadata.len() != entry.size || checksum_file(&path)? != entry.checksum {
            return Err(format!(
                "Backup file '{}' failed checksum validation",
                entry.path
            ));
        }
    }
    Ok(())
}

fn read_sql_snapshot(path: &Path) -> Result<(u64, u64), String> {
    if !path.exists() {
        return Ok((0, 0));
    }
    let snapshot: CatalogSnapshot =
        serde_json::from_slice(&fs::read(path).map_err(|e| e.to_string())?)
            .map_err(|error| format!("Failed to parse SQL catalog snapshot: {error}"))?;
    Ok((snapshot.catalog_version, snapshot.timestamp_millis))
}

fn read_kv_snapshot(path: &Path) -> Result<(u64, u64), String> {
    if !path.exists() {
        return Ok((0, 0));
    }
    let content = fs::read_to_string(path).map_err(|e| e.to_string())?;
    if let Ok(snapshot) = serde_json::from_str::<KvSnapshot>(&content) {
        Ok((snapshot.sequence, snapshot.timestamp_millis))
    } else {
        Ok((0, 0))
    }
}

fn collect_sql_records(roots: &[PathBuf]) -> Result<BTreeMap<u64, WalRecord>, String> {
    let mut records = BTreeMap::new();
    for root in roots {
        for path in list_files(root)? {
            if path.file_name().is_some_and(|name| {
                name.to_string_lossy().contains("sql")
                    && (name.to_string_lossy().ends_with(".wal") || name == "sql_wal.log")
            }) {
                for line in BufReader::new(File::open(&path).map_err(|e| e.to_string())?)
                    .lines()
                    .map_while(Result::ok)
                {
                    if let Ok(record) = serde_json::from_str::<WalRecord>(&line) {
                        if !record.valid() {
                            return Err(format!("Invalid SQL WAL record in {}", path.display()));
                        }
                        records.entry(record.commit_version).or_insert(record);
                    }
                }
            }
        }
    }
    Ok(records)
}

fn collect_kv_records(roots: &[PathBuf]) -> Result<BTreeMap<u64, KvWalRecord>, String> {
    let mut records = BTreeMap::new();
    for root in roots {
        for path in list_files(root)? {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy())
                .unwrap_or_default();
            if name.starts_with("kv-") || name == "rustydb.wal" {
                for line in BufReader::new(File::open(&path).map_err(|e| e.to_string())?)
                    .lines()
                    .map_while(Result::ok)
                {
                    if let Ok(record) = serde_json::from_str::<KvWalRecord>(&line) {
                        if !record.valid() {
                            return Err(format!("Invalid KV WAL record in {}", path.display()));
                        }
                        records.entry(record.sequence).or_insert(record);
                    }
                }
            }
        }
    }
    Ok(records)
}

fn max_sql_version(root: &Path, base: u64) -> u64 {
    collect_sql_records(&[root.to_path_buf()])
        .ok()
        .and_then(|records| records.keys().next_back().copied())
        .unwrap_or(base)
        .max(base)
}
fn max_kv_sequence(root: &Path, base: u64) -> u64 {
    collect_kv_records(&[root.to_path_buf()])
        .ok()
        .and_then(|records| records.keys().next_back().copied())
        .unwrap_or(base)
        .max(base)
}

fn latest_record_timestamp(path: &Path) -> Result<Option<u64>, String> {
    let mut latest = None;
    for line in BufReader::new(File::open(path).map_err(|e| e.to_string())?)
        .lines()
        .map_while(Result::ok)
    {
        if let Ok(record) = serde_json::from_str::<WalRecord>(&line)
            && record.valid()
        {
            latest = Some(latest.unwrap_or(0).max(record.timestamp_millis));
        }
        if let Ok(record) = serde_json::from_str::<KvWalRecord>(&line)
            && record.valid()
        {
            latest = Some(latest.unwrap_or(0).max(record.timestamp_millis));
        }
    }
    Ok(latest)
}

fn legacy_lines(path: &Path, sql: bool) -> Result<Vec<String>, String> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    Ok(BufReader::new(File::open(path).map_err(|e| e.to_string())?)
        .lines()
        .map_while(Result::ok)
        .filter(|line| {
            !line.trim().is_empty()
                && if sql {
                    serde_json::from_str::<WalRecord>(line).is_err()
                } else {
                    serde_json::from_str::<KvWalRecord>(line).is_err()
                }
        })
        .collect())
}

fn ensure_destination_empty(path: &Path) -> Result<(), String> {
    if path.exists()
        && fs::read_dir(path)
            .map_err(|e| e.to_string())?
            .next()
            .is_some()
    {
        return Err(format!("Destination '{}' must be empty", path.display()));
    }
    Ok(())
}

fn copy_file(source: &Path, destination: &Path) -> Result<(), String> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    fs::copy(source, destination)
        .map_err(|error| format!("Failed to copy {}: {error}", source.display()))?;
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(destination)
        .and_then(|file| file.sync_all())
        .map_err(|error| format!("Failed to sync {}: {error}", destination.display()))?;
    Ok(())
}

fn copy_tree(source: &Path, destination: &Path) -> Result<(), String> {
    for path in list_files(source)? {
        copy_file(&path, &destination.join(path.strip_prefix(source).unwrap()))?;
    }
    Ok(())
}

fn list_files(root: &Path) -> Result<Vec<PathBuf>, String> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut result = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        for entry in fs::read_dir(&path)
            .map_err(|error| format!("Failed to read {}: {error}", path.display()))?
        {
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                result.push(path);
            }
        }
    }
    result.sort();
    Ok(result)
}

fn checksum_file(path: &Path) -> Result<u32, String> {
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    let mut hasher = Hasher::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hasher.finalize())
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
fn system_time_millis(time: SystemTime) -> Result<u64, String> {
    time.duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis() as u64)
        .map_err(|_| "Recovery timestamp predates the Unix epoch".to_string())
}

/// Parse an RFC 3339 timestamp, including `Z` and numeric UTC offsets.
pub fn parse_rfc3339(value: &str) -> Result<SystemTime, String> {
    let value = value.trim();
    let (date, time) = value
        .split_once('T')
        .or_else(|| value.split_once('t'))
        .ok_or_else(|| "Timestamp must use RFC 3339 date-time syntax".to_string())?;
    let mut date_parts = date.split('-');
    let year: i32 = date_parts
        .next()
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| "Invalid timestamp year".to_string())?;
    let month: u32 = date_parts
        .next()
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| "Invalid timestamp month".to_string())?;
    let day: u32 = date_parts
        .next()
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| "Invalid timestamp day".to_string())?;
    if date_parts.next().is_some() || !(1..=12).contains(&month) {
        return Err("Invalid timestamp date".to_string());
    }
    let days_in_month = match month {
        2 if is_leap_year(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if !(1..=days_in_month).contains(&day) {
        return Err("Invalid timestamp date".to_string());
    }
    let (clock, offset_seconds) =
        if let Some(clock) = time.strip_suffix('Z').or_else(|| time.strip_suffix('z')) {
            (clock, 0i64)
        } else {
            let offset_index = time
                .char_indices()
                .filter(|(index, c)| *index > 0 && (*c == '+' || *c == '-'))
                .map(|(i, _)| i)
                .next_back()
                .ok_or_else(|| "RFC 3339 timestamp requires Z or a UTC offset".to_string())?;
            let (clock, offset) = time.split_at(offset_index);
            let sign = if offset.starts_with('-') { -1i64 } else { 1 };
            let mut parts = offset[1..].split(':');
            let hours: i64 = parts
                .next()
                .and_then(|v| v.parse().ok())
                .ok_or_else(|| "Invalid timestamp offset".to_string())?;
            let minutes: i64 = parts
                .next()
                .and_then(|v| v.parse().ok())
                .ok_or_else(|| "Invalid timestamp offset".to_string())?;
            if parts.next().is_some() || hours > 23 || minutes > 59 {
                return Err("Invalid timestamp offset".to_string());
            }
            (clock, sign * (hours * 3600 + minutes * 60))
        };
    let mut clock_parts = clock.split(':');
    let hour: i64 = clock_parts
        .next()
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| "Invalid timestamp hour".to_string())?;
    let minute: i64 = clock_parts
        .next()
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| "Invalid timestamp minute".to_string())?;
    let second_part = clock_parts
        .next()
        .ok_or_else(|| "Invalid timestamp second".to_string())?;
    if clock_parts.next().is_some() || hour > 23 || minute > 59 {
        return Err("Invalid timestamp time".to_string());
    }
    let (seconds_text, fraction) = second_part.split_once('.').unwrap_or((second_part, ""));
    let second: i64 = seconds_text
        .parse()
        .map_err(|_| "Invalid timestamp second".to_string())?;
    if second > 59 {
        return Err("Invalid timestamp second".to_string());
    }
    let millis = if fraction.is_empty() {
        if second_part.ends_with('.') {
            return Err("Invalid timestamp fraction".to_string());
        }
        0
    } else {
        if !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err("Invalid timestamp fraction".to_string());
        }
        let digits = fraction.chars().take(3).collect::<String>();
        let parsed: u64 = digits
            .parse()
            .map_err(|_| "Invalid timestamp fraction".to_string())?;
        parsed * 10u64.pow(3 - digits.len() as u32)
    };
    let days = days_from_civil(year, month, day);
    let unix_seconds = days * 86_400 + hour * 3600 + minute * 60 + second - offset_seconds;
    if unix_seconds < 0 {
        return Err("Recovery timestamp predates the Unix epoch".to_string());
    }
    Ok(UNIX_EPOCH
        + std::time::Duration::from_secs(unix_seconds as u64)
        + std::time::Duration::from_millis(millis))
}

fn is_leap_year(year: i32) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let year = year - i32::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month = month as i32;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day as i32 - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    (era * 146097 + day_of_era - 719468) as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ExecutionResult, KVStore, SQLDatabase, Value};
    use tempfile::tempdir;

    #[test]
    fn combined_backup_restore_and_pitr() {
        let source = tempdir().unwrap();
        let backup_parent = tempdir().unwrap();
        let backup = backup_parent.path().join("backup");
        {
            let sql = SQLDatabase::open(source.path().to_str().unwrap()).unwrap();
            sql.execute("CREATE TABLE items (id INT PRIMARY KEY, value TEXT)");
            sql.execute("INSERT INTO items VALUES (1, 'one')");
            sql.checkpoint().unwrap();
            let kv = KVStore::open(source.path().to_str().unwrap()).unwrap();
            kv.set("one".to_string(), "1".to_string()).unwrap();
            kv.snapshot().unwrap();
        }
        let manifest = BackupManager::create(source.path(), &backup).unwrap();
        assert_eq!(manifest.base_point, manifest.recovery_point);

        let later_point;
        {
            let sql = SQLDatabase::open(source.path().to_str().unwrap()).unwrap();
            sql.execute("INSERT INTO items VALUES (2, 'two')");
            sql.checkpoint().unwrap();
            let kv = KVStore::open(source.path().to_str().unwrap()).unwrap();
            kv.set("two".to_string(), "2".to_string()).unwrap();
            kv.snapshot().unwrap();
            later_point = RecoveryPoint {
                sql_commit_version: sql.current_version(),
                kv_sequence: kv.current_sequence().unwrap(),
            };
        }

        let restored = tempdir().unwrap();
        let restored_path = restored.path().join("data");
        let point = BackupManager::restore(
            &backup,
            Some(&source.path().join("wal_archive")),
            &restored_path,
            RecoveryTarget::Point(later_point),
        )
        .unwrap();
        assert_eq!(point, later_point);
        let sql = SQLDatabase::open(restored_path.to_str().unwrap()).unwrap();
        assert_eq!(sql.table_row_count("items"), Some(2));
        let ExecutionResult::Select(result) = sql.execute("SELECT value FROM items WHERE id = 2")
        else {
            panic!("expected row");
        };
        assert_eq!(result.rows[0].values[0], Value::Text("two".to_string()));
        let kv = KVStore::open(restored_path.to_str().unwrap()).unwrap();
        assert_eq!(kv.get("two").unwrap().unwrap().as_str(), "2");
    }

    #[test]
    fn backup_refuses_active_engines_and_restore_validates_checksums() {
        let source = tempdir().unwrap();
        let active = SQLDatabase::open(source.path().to_str().unwrap()).unwrap();
        let backup_parent = tempdir().unwrap();
        let backup = backup_parent.path().join("backup");
        assert!(BackupManager::create(source.path(), &backup).is_err());
        drop(active);
        BackupManager::create(source.path(), &backup).unwrap();
        let manifest: BackupManifest =
            serde_json::from_slice(&fs::read(backup.join("manifest.json")).unwrap()).unwrap();
        if let Some(file) = manifest.files.first() {
            fs::write(backup.join(&file.path), b"corrupt").unwrap();
            let output = tempdir().unwrap().path().join("restore");
            assert!(BackupManager::restore(&backup, None, output, RecoveryTarget::Latest).is_err());
        }
    }

    #[test]
    fn parses_rfc3339_offsets_and_recovery_points() {
        assert_eq!(
            parse_rfc3339("1970-01-01T00:00:01Z")
                .unwrap()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            1
        );
        assert_eq!(
            parse_rfc3339("1970-01-01T08:00:01+08:00")
                .unwrap()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            1
        );
        assert_eq!(
            "sql:12,kv:34".parse::<RecoveryPoint>().unwrap(),
            RecoveryPoint {
                sql_commit_version: 12,
                kv_sequence: 34
            }
        );
        assert!(parse_rfc3339("2024-02-29T00:00:00Z").is_ok());
        assert!(parse_rfc3339("2026-02-29T00:00:00Z").is_err());
        assert!(parse_rfc3339("2026-02-31T00:00:00Z").is_err());
        assert!(parse_rfc3339("2026-04-31T00:00:00Z").is_err());
        assert!(parse_rfc3339("2026-01-01T00:00:00.Z").is_err());
        assert!(parse_rfc3339("2026-01-01T00:00:00.123abcZ").is_err());
    }
}
