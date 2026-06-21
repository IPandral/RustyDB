use crc32fast::Hasher;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use super::executor::{
    Catalog, ExecutionResult, Executor, Table, is_transaction_control, is_write_statement,
};
use super::parser::{Statement, parse_sql};
use super::planner::Optimizer;

const CATALOG_FORMAT_VERSION: u32 = 1;
const WAL_FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone)]
pub struct SQLDatabaseOptions {
    pub plan_cache_capacity: usize,
    pub memory_pool_capacity: usize,
    pub bloom_false_positive_rate: f64,
}

impl Default for SQLDatabaseOptions {
    fn default() -> Self {
        Self {
            plan_cache_capacity: 256,
            memory_pool_capacity: 32,
            bloom_false_positive_rate: 0.01,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SQLConfig {
    pub data_dir: PathBuf,
    pub auto_save: bool,
    pub options: SQLDatabaseOptions,
}

impl SQLConfig {
    pub fn new(data_dir: &str) -> Self {
        Self {
            data_dir: PathBuf::from(data_dir),
            auto_save: true,
            options: SQLDatabaseOptions::default(),
        }
    }

    pub fn memory_only() -> Self {
        Self {
            data_dir: PathBuf::new(),
            auto_save: false,
            options: SQLDatabaseOptions::default(),
        }
    }

    pub fn with_options(mut self, options: SQLDatabaseOptions) -> Self {
        self.options = options;
        self
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct PersistedTable {
    schema: super::types::TableSchema,
    rows: Vec<super::types::Row>,
}

#[derive(Debug, Serialize, Deserialize)]
struct CatalogSnapshot {
    format_version: u32,
    catalog_version: u64,
    #[serde(default)]
    schema_version: u64,
    tables: HashMap<String, Table>,
}

#[derive(Debug, Serialize, Deserialize)]
struct WalRecord {
    format_version: u32,
    base_version: u64,
    commit_version: u64,
    statements: Vec<String>,
    checksum: u32,
}

impl WalRecord {
    fn new(base_version: u64, commit_version: u64, statements: Vec<String>) -> Self {
        let checksum = wal_checksum(base_version, commit_version, &statements);
        Self {
            format_version: WAL_FORMAT_VERSION,
            base_version,
            commit_version,
            statements,
            checksum,
        }
    }

    fn valid(&self) -> bool {
        self.format_version == WAL_FORMAT_VERSION
            && self.checksum
                == wal_checksum(self.base_version, self.commit_version, &self.statements)
    }
}

fn wal_checksum(base_version: u64, commit_version: u64, statements: &[String]) -> u32 {
    let mut hasher = Hasher::new();
    hasher.update(&base_version.to_le_bytes());
    hasher.update(&commit_version.to_le_bytes());
    for statement in statements {
        hasher.update(&(statement.len() as u64).to_le_bytes());
        hasher.update(statement.as_bytes());
    }
    hasher.finalize()
}

struct PlanCacheEntry {
    catalog_version: u64,
    statement: Statement,
    last_used: u64,
}

struct PlanCache {
    capacity: usize,
    clock: u64,
    entries: HashMap<String, PlanCacheEntry>,
}

impl PlanCache {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            clock: 0,
            entries: HashMap::new(),
        }
    }

    fn get(&mut self, sql: &str, catalog_version: u64) -> Option<Statement> {
        self.clock = self.clock.saturating_add(1);
        let entry = self.entries.get_mut(sql)?;
        if entry.catalog_version != catalog_version {
            self.entries.remove(sql);
            return None;
        }
        entry.last_used = self.clock;
        Some(entry.statement.clone())
    }

    fn insert(&mut self, sql: String, catalog_version: u64, statement: Statement) {
        if self.capacity == 0 {
            return;
        }
        self.clock = self.clock.saturating_add(1);
        if self.entries.len() >= self.capacity
            && !self.entries.contains_key(&sql)
            && let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(key, _)| key.clone())
        {
            self.entries.remove(&oldest);
        }
        self.entries.insert(
            sql,
            PlanCacheEntry {
                catalog_version,
                statement,
                last_used: self.clock,
            },
        );
    }

    fn clear(&mut self) {
        self.entries.clear();
    }
}

struct DatabaseInner {
    executor: Executor,
    config: SQLConfig,
    wal: Mutex<Option<File>>,
    plan_cache: Mutex<PlanCache>,
}

/// Main embedded SQL database. Clones share the same catalog and WAL.
#[derive(Clone)]
pub struct SQLDatabase {
    inner: Arc<DatabaseInner>,
}

impl SQLDatabase {
    pub fn new() -> Self {
        Self::with_options(SQLDatabaseOptions::default())
    }

    pub fn with_options(options: SQLDatabaseOptions) -> Self {
        let config = SQLConfig::memory_only().with_options(options.clone());
        let executor = Executor::new();
        executor.configure_bloom_false_positive_rate(options.bloom_false_positive_rate);
        executor.configure_memory_pool_capacity(options.memory_pool_capacity);
        Self {
            inner: Arc::new(DatabaseInner {
                executor,
                config,
                wal: Mutex::new(None),
                plan_cache: Mutex::new(PlanCache::new(options.plan_cache_capacity)),
            }),
        }
    }

    pub fn open(data_dir: &str) -> Result<Self, String> {
        Self::open_with_options(data_dir, SQLDatabaseOptions::default())
    }

    pub fn open_with_options(data_dir: &str, options: SQLDatabaseOptions) -> Result<Self, String> {
        let config = SQLConfig::new(data_dir).with_options(options.clone());
        fs::create_dir_all(&config.data_dir)
            .map_err(|error| format!("Failed to create data directory: {error}"))?;

        let mut catalog = Self::load_catalog(&config)?;
        let wal_path = config.data_dir.join("sql_wal.log");
        if wal_path.exists() {
            Self::replay_wal(&wal_path, &mut catalog)?;
        }
        let wal = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&wal_path)
            .map_err(|error| format!("Failed to open WAL: {error}"))?;

        let executor = Executor::from_catalog(catalog);
        executor.configure_bloom_false_positive_rate(options.bloom_false_positive_rate);
        executor.configure_memory_pool_capacity(options.memory_pool_capacity);
        Ok(Self {
            inner: Arc::new(DatabaseInner {
                executor,
                config,
                wal: Mutex::new(Some(wal)),
                plan_cache: Mutex::new(PlanCache::new(options.plan_cache_capacity)),
            }),
        })
    }

    pub fn options(&self) -> &SQLDatabaseOptions {
        &self.inner.config.options
    }

    pub fn session(&self) -> SQLSession {
        SQLSession {
            database: self.clone(),
            transaction: Mutex::new(None),
        }
    }

    /// Execute one statement in autocommit mode.
    pub fn execute(&self, sql: &str) -> ExecutionResult {
        let statement = match self.parse_cached(sql) {
            Ok(statement) => statement,
            Err(error) => return ExecutionResult::Error(error),
        };
        if is_transaction_control(&statement) {
            return ExecutionResult::Error(
                "BEGIN/COMMIT/ROLLBACK require SQLDatabase::session()".to_string(),
            );
        }
        self.execute_autocommit(sql, statement)
    }

    fn parse_cached(&self, sql: &str) -> Result<Statement, String> {
        let normalized = sql.trim().trim_end_matches(';').trim().to_string();
        let version = self.inner.executor.snapshot().schema_version;
        if let Some(statement) = self
            .inner
            .plan_cache
            .lock()
            .map_err(|_| "Plan cache lock poisoned".to_string())?
            .get(&normalized, version)
        {
            return Ok(statement);
        }
        let statement = Optimizer::optimize_statement(parse_sql(&normalized)?);
        self.inner
            .plan_cache
            .lock()
            .map_err(|_| "Plan cache lock poisoned".to_string())?
            .insert(normalized, version, statement.clone());
        Ok(statement)
    }

    fn execute_autocommit(&self, sql: &str, statement: Statement) -> ExecutionResult {
        if !is_write_statement(&statement) {
            let mut snapshot = self.inner.executor.snapshot();
            return Executor::execute_catalog(&mut snapshot, statement);
        }

        let catalog_handle = self.inner.executor.catalog_handle();
        let mut catalog = catalog_handle.write().expect("catalog lock poisoned");
        let base_version = catalog.version;
        let mut working = catalog.clone();
        let result = Executor::execute_catalog(&mut working, statement.clone());
        if matches!(result, ExecutionResult::Error(_)) {
            return result;
        }
        working.configure_bloom_false_positive_rate(
            self.inner.config.options.bloom_false_positive_rate,
        );
        if is_schema_statement(&statement) {
            working.schema_version = catalog.schema_version.saturating_add(1);
        }
        working.version = base_version.saturating_add(1);
        if let Err(error) =
            self.log_transaction(base_version, working.version, vec![sql.trim().to_string()])
        {
            return ExecutionResult::Error(format!("WAL error: {error}"));
        }
        *catalog = working;
        drop(catalog);

        if is_schema_statement(&statement)
            && self.inner.config.auto_save
            && let Err(error) = self.save()
        {
            return ExecutionResult::Error(format!(
                "Statement committed but snapshot failed: {error}"
            ));
        }
        result
    }

    pub fn execute_batch(&self, sql: &str) -> Vec<ExecutionResult> {
        let session = self.session();
        split_statements(sql)
            .into_iter()
            .map(|statement| session.execute(&statement))
            .collect()
    }

    fn log_transaction(
        &self,
        base_version: u64,
        commit_version: u64,
        statements: Vec<String>,
    ) -> Result<(), String> {
        let record = WalRecord::new(base_version, commit_version, statements);
        let encoded = serde_json::to_string(&record)
            .map_err(|error| format!("Failed to serialize WAL record: {error}"))?;
        let mut guard = self
            .inner
            .wal
            .lock()
            .map_err(|_| "WAL lock poisoned".to_string())?;
        if let Some(file) = guard.as_mut() {
            writeln!(file, "{encoded}").map_err(|error| format!("Failed to write WAL: {error}"))?;
            file.sync_all()
                .map_err(|error| format!("Failed to sync WAL: {error}"))?;
        }
        Ok(())
    }

    fn replay_wal(path: &Path, catalog: &mut Catalog) -> Result<(), String> {
        let file = File::open(path).map_err(|error| format!("Failed to open WAL: {error}"))?;
        for line in BufReader::new(file).lines() {
            let line = line.map_err(|error| format!("Failed to read WAL: {error}"))?;
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(record) = serde_json::from_str::<WalRecord>(&line) {
                if !record.valid() {
                    // A checksum mismatch is treated as a partial trailing record.
                    break;
                }
                if record.commit_version <= catalog.version {
                    continue;
                }
                if record.base_version != catalog.version {
                    return Err(format!(
                        "WAL version gap: catalog={}, record base={}",
                        catalog.version, record.base_version
                    ));
                }
                for sql in record.statements {
                    let statement = parse_sql(&sql)
                        .map_err(|error| format!("Failed to parse WAL statement: {error}"))?;
                    let schema_statement = is_schema_statement(&statement);
                    let result = Executor::execute_catalog(catalog, statement);
                    if let ExecutionResult::Error(error) = result {
                        return Err(format!("Failed to replay WAL statement: {error}"));
                    }
                    if schema_statement {
                        catalog.schema_version = catalog.schema_version.saturating_add(1);
                    }
                }
                catalog.version = record.commit_version;
                continue;
            }

            // Legacy v0.2 WAL entries were one SQL statement per line.
            match parse_sql(&line) {
                Ok(statement) => {
                    let schema_statement = is_schema_statement(&statement);
                    let result = Executor::execute_catalog(catalog, statement);
                    if !matches!(result, ExecutionResult::Error(_)) {
                        catalog.version = catalog.version.saturating_add(1);
                        if schema_statement {
                            catalog.schema_version = catalog.schema_version.saturating_add(1);
                        }
                    }
                }
                Err(_) => {
                    // Legacy crash recovery skipped malformed/partial trailing lines.
                    break;
                }
            }
        }
        Ok(())
    }

    fn load_catalog(config: &SQLConfig) -> Result<Catalog, String> {
        let catalog_path = config.data_dir.join("catalog.json");
        if catalog_path.exists() {
            let content = fs::read_to_string(&catalog_path)
                .map_err(|error| format!("Failed to read catalog: {error}"))?;
            let snapshot: CatalogSnapshot = serde_json::from_str(&content)
                .map_err(|error| format!("Failed to parse catalog: {error}"))?;
            if snapshot.format_version != CATALOG_FORMAT_VERSION {
                return Err(format!(
                    "Unsupported catalog format {}",
                    snapshot.format_version
                ));
            }
            let mut catalog = Catalog::from_tables(snapshot.tables);
            catalog.version = snapshot.catalog_version;
            catalog.schema_version = snapshot.schema_version;
            return Ok(catalog);
        }

        // Legacy migration: v0.2 stored one JSON file per table.
        let mut tables = HashMap::new();
        let tables_dir = config.data_dir.join("tables");
        if tables_dir.exists() {
            for entry in fs::read_dir(&tables_dir)
                .map_err(|error| format!("Failed to read tables directory: {error}"))?
            {
                let path = entry
                    .map_err(|error| format!("Failed to read table entry: {error}"))?
                    .path();
                if path
                    .extension()
                    .is_some_and(|extension| extension == "json")
                {
                    let content = fs::read_to_string(&path)
                        .map_err(|error| format!("Failed to read table file: {error}"))?;
                    let persisted: PersistedTable = serde_json::from_str(&content)
                        .map_err(|error| format!("Failed to parse legacy table: {error}"))?;
                    let mut table = Table::new(persisted.schema.clone());
                    table.rows = persisted.rows;
                    table.migrate_legacy_rows();
                    tables.insert(persisted.schema.name.clone(), table);
                }
            }
        }
        Ok(Catalog::from_tables(tables))
    }

    pub fn save(&self) -> Result<(), String> {
        if !self.inner.config.auto_save {
            return Ok(());
        }
        // Hold the catalog write lock through snapshot publication and WAL
        // truncation so a concurrent commit cannot land between those steps.
        let catalog_handle = self.inner.executor.catalog_handle();
        let catalog = catalog_handle.write().expect("catalog lock poisoned");
        let snapshot = CatalogSnapshot {
            format_version: CATALOG_FORMAT_VERSION,
            catalog_version: catalog.version,
            schema_version: catalog.schema_version,
            tables: catalog.owned_tables(),
        };
        let encoded = serde_json::to_vec_pretty(&snapshot)
            .map_err(|error| format!("Failed to serialize catalog: {error}"))?;
        let path = self.inner.config.data_dir.join("catalog.json");
        let temporary = self.inner.config.data_dir.join("catalog.json.tmp");
        let mut file = File::create(&temporary)
            .map_err(|error| format!("Failed to create catalog snapshot: {error}"))?;
        file.write_all(&encoded)
            .map_err(|error| format!("Failed to write catalog snapshot: {error}"))?;
        file.sync_all()
            .map_err(|error| format!("Failed to sync catalog snapshot: {error}"))?;
        fs::rename(&temporary, &path)
            .map_err(|error| format!("Failed to publish catalog snapshot: {error}"))?;

        let mut wal = self
            .inner
            .wal
            .lock()
            .map_err(|_| "WAL lock poisoned".to_string())?;
        if let Some(file) = wal.as_mut() {
            file.set_len(0)
                .map_err(|error| format!("Failed to truncate WAL: {error}"))?;
            file.sync_all()
                .map_err(|error| format!("Failed to sync empty WAL: {error}"))?;
        }
        drop(catalog);
        Ok(())
    }

    pub fn checkpoint(&self) -> Result<(), String> {
        self.save()
    }

    pub fn list_tables(&self) -> Vec<String> {
        let mut tables: Vec<String> = self
            .inner
            .executor
            .snapshot()
            .tables
            .keys()
            .cloned()
            .collect();
        tables.sort();
        tables
    }

    pub fn table_row_count(&self, table_name: &str) -> Option<usize> {
        self.inner
            .executor
            .snapshot()
            .tables
            .get(&table_name.to_ascii_lowercase())
            .map(|table| table.rows.len())
    }

    pub fn executor(&self) -> &Executor {
        &self.inner.executor
    }
}

impl Default for SQLDatabase {
    fn default() -> Self {
        Self::new()
    }
}

struct TransactionState {
    base_version: u64,
    catalog: Catalog,
    statements: Vec<String>,
    schema_changed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionTransactionState {
    Autocommit,
    Active,
}

/// Connection-local SQL state with optimistic serializable transactions.
pub struct SQLSession {
    database: SQLDatabase,
    transaction: Mutex<Option<TransactionState>>,
}

impl SQLSession {
    pub fn execute(&self, sql: &str) -> ExecutionResult {
        let statement = match self.database.parse_cached(sql) {
            Ok(statement) => statement,
            Err(error) => return ExecutionResult::Error(error),
        };
        match statement {
            Statement::Begin => return self.begin(),
            Statement::Commit => return self.commit(),
            Statement::Rollback => return self.rollback(),
            _ => {}
        }

        let mut transaction = self.transaction.lock().expect("transaction lock poisoned");
        if let Some(state) = transaction.as_mut() {
            let result = Executor::execute_catalog(&mut state.catalog, statement.clone());
            if !matches!(result, ExecutionResult::Error(_)) && is_write_statement(&statement) {
                state.statements.push(sql.trim().to_string());
                state.schema_changed |= is_schema_statement(&statement);
            }
            result
        } else {
            drop(transaction);
            self.database.execute_autocommit(sql, statement)
        }
    }

    pub fn begin(&self) -> ExecutionResult {
        let mut transaction = self.transaction.lock().expect("transaction lock poisoned");
        if transaction.is_some() {
            return ExecutionResult::Error("A transaction is already active".to_string());
        }
        let catalog = self.database.inner.executor.snapshot();
        *transaction = Some(TransactionState {
            base_version: catalog.version,
            catalog,
            statements: Vec::new(),
            schema_changed: false,
        });
        ExecutionResult::TransactionStarted
    }

    pub fn commit(&self) -> ExecutionResult {
        let mut transaction = self.transaction.lock().expect("transaction lock poisoned");
        let Some(mut state) = transaction.take() else {
            // MySQL clients commonly issue COMMIT after autocommit statements.
            // Treat that as a successful no-op for wire-protocol compatibility.
            return ExecutionResult::TransactionCommitted;
        };
        let catalog_handle = self.database.inner.executor.catalog_handle();
        let mut catalog = catalog_handle.write().expect("catalog lock poisoned");
        if catalog.version != state.base_version {
            return ExecutionResult::Error(format!(
                "Serialization conflict: transaction snapshot version {}, current version {}",
                state.base_version, catalog.version
            ));
        }
        if state.statements.is_empty() {
            return ExecutionResult::TransactionCommitted;
        }
        state.catalog.configure_bloom_false_positive_rate(
            self.database.inner.config.options.bloom_false_positive_rate,
        );
        state.catalog.schema_version = if state.schema_changed {
            catalog.schema_version.saturating_add(1)
        } else {
            catalog.schema_version
        };
        state.catalog.version = catalog.version.saturating_add(1);
        if let Err(error) = self.database.log_transaction(
            state.base_version,
            state.catalog.version,
            state.statements.clone(),
        ) {
            // Restore transaction state so the caller may retry COMMIT or ROLLBACK.
            *transaction = Some(state);
            return ExecutionResult::Error(format!("WAL error: {error}"));
        }
        *catalog = state.catalog;
        self.database
            .inner
            .plan_cache
            .lock()
            .expect("plan cache lock poisoned")
            .clear();
        ExecutionResult::TransactionCommitted
    }

    pub fn rollback(&self) -> ExecutionResult {
        let mut transaction = self.transaction.lock().expect("transaction lock poisoned");
        if transaction.take().is_none() {
            // As with COMMIT, ROLLBACK outside an explicit transaction is a
            // successful no-op in MySQL-compatible clients.
            return ExecutionResult::TransactionRolledBack;
        }
        ExecutionResult::TransactionRolledBack
    }

    pub fn transaction_state(&self) -> SessionTransactionState {
        if self
            .transaction
            .lock()
            .expect("transaction lock poisoned")
            .is_some()
        {
            SessionTransactionState::Active
        } else {
            SessionTransactionState::Autocommit
        }
    }

    pub fn is_in_transaction(&self) -> bool {
        self.transaction_state() == SessionTransactionState::Active
    }
}

fn is_schema_statement(statement: &Statement) -> bool {
    matches!(
        statement,
        Statement::CreateTable { .. }
            | Statement::DropTable { .. }
            | Statement::CreateIndex { .. }
            | Statement::DropIndex { .. }
    )
}

fn split_statements(sql: &str) -> Vec<String> {
    // The parser handles semicolons in string literals, but the batch API remains
    // intentionally simple and mirrors the original public behavior.
    sql.split(';')
        .map(str::trim)
        .filter(|statement| !statement.is_empty())
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sql::Value;
    use tempfile::tempdir;

    #[test]
    fn autocommit_and_batch_compatibility() {
        let database = SQLDatabase::new();
        let results = database.execute_batch(
            "CREATE TABLE items (id INT PRIMARY KEY, name TEXT);
             INSERT INTO items VALUES (1, 'apple'), (2, 'banana');
             SELECT COUNT(*) AS count FROM items;",
        );
        assert_eq!(results.len(), 3);
        match &results[2] {
            ExecutionResult::Select(result) => {
                assert_eq!(result.rows[0].values[0], Value::Integer(2));
            }
            result => panic!("unexpected result: {result:?}"),
        }
    }

    #[test]
    fn transaction_commit_rollback_and_conflict() {
        let database = SQLDatabase::new();
        database.execute("CREATE TABLE test (id INT PRIMARY KEY, value TEXT)");
        let first = database.session();
        let second = database.session();
        assert!(matches!(
            first.commit(),
            ExecutionResult::TransactionCommitted
        ));
        assert!(matches!(
            first.rollback(),
            ExecutionResult::TransactionRolledBack
        ));
        assert!(matches!(first.begin(), ExecutionResult::TransactionStarted));
        first.execute("INSERT INTO test VALUES (1, 'one')");
        assert_eq!(database.table_row_count("test"), Some(0));
        assert!(matches!(
            first.commit(),
            ExecutionResult::TransactionCommitted
        ));
        assert_eq!(database.table_row_count("test"), Some(1));

        first.begin();
        second.begin();
        first.execute("INSERT INTO test VALUES (2, 'two')");
        second.execute("INSERT INTO test VALUES (3, 'three')");
        assert!(matches!(
            first.commit(),
            ExecutionResult::TransactionCommitted
        ));
        assert!(matches!(second.commit(), ExecutionResult::Error(_)));

        first.begin();
        first.execute("DELETE FROM test");
        first.rollback();
        assert_eq!(database.table_row_count("test"), Some(2));
    }

    #[test]
    fn versioned_persistence_and_partial_wal_recovery() {
        let directory = tempdir().unwrap();
        let path = directory.path().to_str().unwrap();
        {
            let database = SQLDatabase::open(path).unwrap();
            database.execute("CREATE TABLE test (id INT PRIMARY KEY, value TEXT)");
            database.execute("INSERT INTO test VALUES (1, 'one')");
        }
        {
            let database = SQLDatabase::open(path).unwrap();
            assert_eq!(database.table_row_count("test"), Some(1));
            database.save().unwrap();
        }
        let wal_path = directory.path().join("sql_wal.log");
        fs::write(&wal_path, "{\"format_version\":1,\"partial\":true")
            .expect("write partial record");
        let database = SQLDatabase::open(path).unwrap();
        assert_eq!(database.table_row_count("test"), Some(1));
    }

    #[test]
    fn legacy_table_migration_rebuilds_stable_indexes() {
        let directory = tempdir().unwrap();
        let tables_directory = directory.path().join("tables");
        fs::create_dir_all(&tables_directory).unwrap();
        let schema = super::super::types::TableSchema::new(
            "legacy",
            vec![
                super::super::types::ColumnDef::new("id", super::super::types::DataType::Integer)
                    .primary_key(),
                super::super::types::ColumnDef::new("value", super::super::types::DataType::Text),
            ],
        );
        let persisted = PersistedTable {
            schema,
            rows: vec![super::super::types::Row::new(vec![
                Value::Integer(1),
                Value::Text("legacy".to_string()),
            ])],
        };
        fs::write(
            tables_directory.join("legacy.json"),
            serde_json::to_vec_pretty(&persisted).unwrap(),
        )
        .unwrap();

        let database = SQLDatabase::open(directory.path().to_str().unwrap()).unwrap();
        assert_eq!(database.table_row_count("legacy"), Some(1));
        match database.execute("SELECT value FROM legacy WHERE id = 1") {
            ExecutionResult::Select(result) => {
                assert_eq!(result.rows[0].values[0], Value::Text("legacy".to_string()));
            }
            result => panic!("unexpected result: {result:?}"),
        }
        database.save().unwrap();
        assert!(directory.path().join("catalog.json").exists());
        assert!(
            tables_directory.join("legacy.json").exists(),
            "legacy files must not be deleted automatically"
        );
    }

    #[test]
    fn transactional_ddl_rolls_back_and_indexes_survive_reopen() {
        let directory = tempdir().unwrap();
        let path = directory.path().to_str().unwrap();
        {
            let database = SQLDatabase::open(path).unwrap();
            let session = database.session();
            session.begin();
            session.execute("CREATE TABLE rolled_back (id INT)");
            session.rollback();
            assert!(!database.list_tables().contains(&"rolled_back".to_string()));

            database.execute("CREATE TABLE indexed (category TEXT, sequence INT)");
            database.execute("INSERT INTO indexed VALUES ('a', 1), ('a', 2), ('b', 3)");
            database
                .execute("CREATE INDEX indexed_category_sequence ON indexed(category, sequence)");
            database.save().unwrap();
        }
        let database = SQLDatabase::open(path).unwrap();
        match database
            .execute("EXPLAIN SELECT * FROM indexed WHERE category = 'a' AND sequence >= 2")
        {
            ExecutionResult::Explain(plan) => {
                assert!(plan.join("\n").contains("indexed_category_sequence"));
            }
            result => panic!("unexpected result: {result:?}"),
        }
        match database.execute("SELECT * FROM indexed WHERE category = 'missing'") {
            ExecutionResult::Select(result) => assert!(result.rows.is_empty()),
            result => panic!("unexpected result: {result:?}"),
        }
    }
}
