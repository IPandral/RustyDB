use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use super::executor::{Executor, ExecutionResult, Table};
use super::parser::{parse_sql, Statement};
use super::types::*;

/// SQL Database configuration
#[derive(Debug, Clone)]
pub struct SQLConfig {
    pub data_dir: PathBuf,
    pub auto_save: bool,
}

impl SQLConfig {
    pub fn new(data_dir: &str) -> Self {
        SQLConfig {
            data_dir: PathBuf::from(data_dir),
            auto_save: true,
        }
    }

    pub fn memory_only() -> Self {
        SQLConfig {
            data_dir: PathBuf::new(),
            auto_save: false,
        }
    }
}

/// Persisted table data
#[derive(Debug, Serialize, Deserialize)]
struct PersistedTable {
    schema: TableSchema,
    rows: Vec<Row>,
}

/// The main SQL Database
pub struct SQLDatabase {
    executor: Executor,
    config: SQLConfig,
    wal: Arc<Mutex<Option<File>>>,
}

impl SQLDatabase {
    /// Create a new in-memory SQL database
    pub fn new() -> Self {
        SQLDatabase {
            executor: Executor::new(),
            config: SQLConfig::memory_only(),
            wal: Arc::new(Mutex::new(None)),
        }
    }

    /// Create a SQL database with persistence
    pub fn open(data_dir: &str) -> Result<Self, String> {
        let config = SQLConfig::new(data_dir);
        
        // Create data directory
        fs::create_dir_all(&config.data_dir)
            .map_err(|e| format!("Failed to create data directory: {}", e))?;

        // Load existing tables
        let tables = Self::load_tables(&config)?;
        let executor = Executor::with_tables(tables);

        // Replay WAL if exists
        let wal_path = config.data_dir.join("sql_wal.log");
        if wal_path.exists() {
            Self::replay_wal(&wal_path, &executor)?;
        }

        // Open WAL for writing
        let wal_file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&wal_path)
            .map_err(|e| format!("Failed to open WAL: {}", e))?;

        Ok(SQLDatabase {
            executor,
            config,
            wal: Arc::new(Mutex::new(Some(wal_file))),
        })
    }

    /// Execute a SQL string
    pub fn execute(&self, sql: &str) -> ExecutionResult {
        // Parse SQL
        let statement = match parse_sql(sql) {
            Ok(stmt) => stmt,
            Err(e) => return ExecutionResult::Error(e),
        };

        // Log to WAL if it's a write operation
        if self.is_write_operation(&statement) {
            if let Err(e) = self.log_to_wal(sql) {
                return ExecutionResult::Error(format!("WAL error: {}", e));
            }
        }

        // Execute
        let result = self.executor.execute(statement);

        // Auto-save on schema changes
        if self.config.auto_save {
            if let ExecutionResult::TableCreated(_) | ExecutionResult::TableDropped(_) = &result {
                if let Err(e) = self.save() {
                    eprintln!("Warning: Failed to auto-save: {}", e);
                }
            }
        }

        result
    }

    /// Execute multiple SQL statements separated by semicolons
    #[allow(dead_code)]
    pub fn execute_batch(&self, sql: &str) -> Vec<ExecutionResult> {
        let statements: Vec<&str> = sql
            .split(';')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();

        statements.iter().map(|s| self.execute(s)).collect()
    }

    /// Check if a statement is a write operation
    fn is_write_operation(&self, statement: &Statement) -> bool {
        matches!(
            statement,
            Statement::Insert { .. }
                | Statement::Update { .. }
                | Statement::Delete { .. }
                | Statement::CreateTable { .. }
                | Statement::DropTable { .. }
        )
    }

    /// Log SQL to WAL
    fn log_to_wal(&self, sql: &str) -> Result<(), String> {
        if let Ok(mut guard) = self.wal.lock() {
            if let Some(ref mut file) = *guard {
                writeln!(file, "{}", sql)
                    .map_err(|e| format!("Failed to write to WAL: {}", e))?;
                file.sync_all()
                    .map_err(|e| format!("Failed to sync WAL: {}", e))?;
            }
        }
        Ok(())
    }

    /// Replay WAL from file
    fn replay_wal(wal_path: &PathBuf, executor: &Executor) -> Result<(), String> {
        let file = File::open(wal_path)
            .map_err(|e| format!("Failed to open WAL: {}", e))?;
        let reader = BufReader::new(file);

        for line in reader.lines() {
            let sql = line.map_err(|e| format!("Failed to read WAL line: {}", e))?;
            if sql.trim().is_empty() {
                continue;
            }
            
            match parse_sql(&sql) {
                Ok(stmt) => {
                    let _ = executor.execute(stmt);
                }
                Err(e) => {
                    eprintln!("Warning: Failed to replay WAL entry '{}': {}", sql, e);
                }
            }
        }

        Ok(())
    }

    /// Load tables from disk
    fn load_tables(config: &SQLConfig) -> Result<HashMap<String, Table>, String> {
        let mut tables = HashMap::new();
        let tables_dir = config.data_dir.join("tables");

        if !tables_dir.exists() {
            return Ok(tables);
        }

        let entries = fs::read_dir(&tables_dir)
            .map_err(|e| format!("Failed to read tables directory: {}", e))?;

        for entry in entries {
            let entry = entry.map_err(|e| format!("Failed to read directory entry: {}", e))?;
            let path = entry.path();

            if path.extension().map(|e| e == "json").unwrap_or(false) {
                let content = fs::read_to_string(&path)
                    .map_err(|e| format!("Failed to read table file: {}", e))?;

                let persisted: PersistedTable = serde_json::from_str(&content)
                    .map_err(|e| format!("Failed to parse table file: {}", e))?;

                let mut table = Table::new(persisted.schema.clone());
                table.rows = persisted.rows;
                table.rebuild_index();

                tables.insert(persisted.schema.name.clone(), table);
            }
        }

        Ok(tables)
    }

    /// Save all tables to disk
    pub fn save(&self) -> Result<(), String> {
        if !self.config.auto_save {
            return Ok(());
        }

        let tables_dir = self.config.data_dir.join("tables");
        fs::create_dir_all(&tables_dir)
            .map_err(|e| format!("Failed to create tables directory: {}", e))?;

        let tables = self.executor.get_tables();

        for (name, table) in tables {
            let persisted = PersistedTable {
                schema: table.schema.clone(),
                rows: table.rows.clone(),
            };

            let json = serde_json::to_string_pretty(&persisted)
                .map_err(|e| format!("Failed to serialize table: {}", e))?;

            let path = tables_dir.join(format!("{}.json", name));
            fs::write(&path, json)
                .map_err(|e| format!("Failed to write table file: {}", e))?;
        }

        // Clear WAL after successful save
        if let Ok(mut guard) = self.wal.lock() {
            if let Some(ref mut file) = *guard {
                file.set_len(0).ok();
            }
        }

        Ok(())
    }

    /// Compact WAL by saving current state and clearing WAL
    #[allow(dead_code)]
    pub fn checkpoint(&self) -> Result<(), String> {
        self.save()
    }

    /// Get list of tables
    pub fn list_tables(&self) -> Vec<String> {
        self.executor
            .get_tables()
            .keys()
            .cloned()
            .collect()
    }

    /// Get row count for a table
    #[allow(dead_code)]
    pub fn table_row_count(&self, table_name: &str) -> Option<usize> {
        let tables = self.executor.get_tables();
        tables.get(table_name).map(|t| t.rows.len())
    }

    /// Get the executor (for advanced usage)
    #[allow(dead_code)]
    pub fn executor(&self) -> &Executor {
        &self.executor
    }
}

impl Default for SQLDatabase {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for SQLDatabase {
    fn clone(&self) -> Self {
        SQLDatabase {
            executor: self.executor.clone(),
            config: self.config.clone(),
            wal: Arc::new(Mutex::new(None)), // WAL is not cloned
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_basic_sql() {
        let db = SQLDatabase::new();
        
        db.execute("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, age INTEGER)");
        db.execute("INSERT INTO users VALUES (1, 'Alice', 30)");
        db.execute("INSERT INTO users VALUES (2, 'Bob', 25)");
        
        let result = db.execute("SELECT * FROM users");
        if let ExecutionResult::Select(rs) = result {
            assert_eq!(rs.row_count(), 2);
        } else {
            panic!("Expected Select result");
        }
    }

    #[test]
    fn test_mysql_like_syntax() {
        let db = SQLDatabase::new();
        
        // MySQL-style table creation
        db.execute("CREATE TABLE products (
            id INT PRIMARY KEY,
            name VARCHAR(100) NOT NULL,
            price DECIMAL,
            in_stock BOOLEAN
        )");
        
        // Multiple inserts
        db.execute("INSERT INTO products VALUES (1, 'Laptop', 999.99, TRUE)");
        db.execute("INSERT INTO products VALUES (2, 'Mouse', 29.99, TRUE)");
        db.execute("INSERT INTO products VALUES (3, 'Keyboard', 79.99, FALSE)");
        
        // Query with conditions
        let result = db.execute("SELECT name, price FROM products WHERE price > 50 ORDER BY price DESC");
        if let ExecutionResult::Select(rs) = result {
            assert_eq!(rs.row_count(), 2);
        }
    }

    #[test]
    fn test_persistence() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path().to_str().unwrap();

        // Create and populate
        {
            let db = SQLDatabase::open(data_dir).unwrap();
            db.execute("CREATE TABLE test (id INTEGER PRIMARY KEY, data TEXT)");
            db.execute("INSERT INTO test VALUES (1, 'hello')");
            db.save().unwrap();
        }

        // Reopen and verify
        {
            let db = SQLDatabase::open(data_dir).unwrap();
            let result = db.execute("SELECT * FROM test");
            if let ExecutionResult::Select(rs) = result {
                assert_eq!(rs.row_count(), 1);
            } else {
                panic!("Expected Select result");
            }
        }
    }

    #[test]
    fn test_batch_execute() {
        let db = SQLDatabase::new();
        
        let results = db.execute_batch("
            CREATE TABLE items (id INTEGER, name TEXT);
            INSERT INTO items VALUES (1, 'apple');
            INSERT INTO items VALUES (2, 'banana');
            SELECT * FROM items
        ");
        
        assert_eq!(results.len(), 4);
        if let ExecutionResult::Select(rs) = &results[3] {
            assert_eq!(rs.row_count(), 2);
        }
    }
}
