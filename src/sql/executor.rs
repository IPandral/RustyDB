use std::collections::HashMap;
use std::sync::Arc;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};

use super::types::*;
use super::parser::*;

/// A table in the database
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Table {
    pub schema: TableSchema,
    pub rows: Vec<Row>,
    #[serde(skip)]
    pub primary_key_index: HashMap<String, usize>, // Maps PK value string to row index
}

impl Table {
    pub fn new(schema: TableSchema) -> Self {
        Table {
            schema,
            rows: Vec::new(),
            primary_key_index: HashMap::new(),
        }
    }

    /// Insert a row into the table
    pub fn insert(&mut self, row: Row) -> Result<(), String> {
        self.schema.validate_row(&row.values)?;

        // Check primary key uniqueness
        if let Some(pk_idx) = self.schema.primary_key_index() {
            let pk_value = row.values[pk_idx].to_string();
            if self.primary_key_index.contains_key(&pk_value) {
                return Err(format!("Duplicate primary key: {}", pk_value));
            }
            let row_idx = self.rows.len();
            self.primary_key_index.insert(pk_value, row_idx);
        }

        self.rows.push(row);
        Ok(())
    }

    /// Rebuild the primary key index (after loading from disk)
    pub fn rebuild_index(&mut self) {
        self.primary_key_index.clear();
        if let Some(pk_idx) = self.schema.primary_key_index() {
            for (row_idx, row) in self.rows.iter().enumerate() {
                let pk_value = row.values[pk_idx].to_string();
                self.primary_key_index.insert(pk_value, row_idx);
            }
        }
    }

    /// Check if a row matches a WHERE clause (static version to avoid borrow issues)
    fn row_matches_static(schema: &TableSchema, row: &Row, where_clause: &WhereClause) -> bool {
        if where_clause.conditions.is_empty() {
            return true;
        }

        let mut result = Self::evaluate_condition_static(schema, row, &where_clause.conditions[0]);

        for (i, op) in where_clause.logical_ops.iter().enumerate() {
            let next_result = Self::evaluate_condition_static(schema, row, &where_clause.conditions[i + 1]);
            result = match op {
                LogicalOp::And => result && next_result,
                LogicalOp::Or => result || next_result,
            };
        }

        result
    }

    fn evaluate_condition_static(schema: &TableSchema, row: &Row, condition: &Condition) -> bool {
        if let Some(col_idx) = schema.column_index(&condition.column) {
            if let Some(value) = row.values.get(col_idx) {
                return value.compare(&condition.value, &condition.op);
            }
        }
        false
    }

    /// Check if a row matches a WHERE clause
    fn row_matches(&self, row: &Row, where_clause: &WhereClause) -> bool {
        Self::row_matches_static(&self.schema, row, where_clause)
    }

    /// Select rows matching an optional WHERE clause
    pub fn select(
        &self,
        columns: &SelectColumns,
        where_clause: Option<&WhereClause>,
        order_by: Option<&OrderBy>,
        limit: Option<usize>,
    ) -> Result<ResultSet, String> {
        // Determine output columns
        let output_columns: Vec<String> = match columns {
            SelectColumns::All => self.schema.columns.iter().map(|c| c.name.clone()).collect(),
            SelectColumns::Columns(cols) => {
                // Validate that all columns exist
                for col in cols {
                    if self.schema.column_index(col).is_none() {
                        return Err(format!("Unknown column: {}", col));
                    }
                }
                cols.clone()
            }
        };

        let column_indices: Vec<usize> = output_columns
            .iter()
            .filter_map(|name| self.schema.column_index(name))
            .collect();

        let mut result = ResultSet::new(output_columns);

        // Filter rows
        let mut matching_rows: Vec<&Row> = self
            .rows
            .iter()
            .filter(|row| {
                if let Some(wc) = where_clause {
                    self.row_matches(row, wc)
                } else {
                    true
                }
            })
            .collect();

        // Sort if ORDER BY specified
        if let Some(order) = order_by {
            if let Some(sort_idx) = self.schema.column_index(&order.column) {
                matching_rows.sort_by(|a, b| {
                    let cmp = a.values[sort_idx].partial_cmp(&b.values[sort_idx]);
                    if order.descending {
                        cmp.unwrap_or(std::cmp::Ordering::Equal).reverse()
                    } else {
                        cmp.unwrap_or(std::cmp::Ordering::Equal)
                    }
                });
            }
        }

        // Apply LIMIT
        let rows_to_take = limit.unwrap_or(matching_rows.len());

        // Build result set
        for row in matching_rows.into_iter().take(rows_to_take) {
            let values: Vec<Value> = column_indices
                .iter()
                .map(|&idx| row.values[idx].clone())
                .collect();
            result.add_row(Row::new(values));
        }

        Ok(result)
    }

    /// Update rows matching a WHERE clause
    pub fn update(
        &mut self,
        assignments: &[(String, Value)],
        where_clause: Option<&WhereClause>,
    ) -> Result<usize, String> {
        // Validate assignments
        for (col_name, value) in assignments {
            if let Some(col) = self.schema.column(col_name) {
                if !value.is_compatible_with(&col.data_type) {
                    return Err(format!(
                        "Type mismatch for column '{}': expected {:?}, got {:?}",
                        col_name,
                        col.data_type,
                        value.data_type()
                    ));
                }
            } else {
                return Err(format!("Unknown column: {}", col_name));
            }
        }

        let mut updated_count = 0;

        // Clone schema to avoid borrow issues
        let schema = self.schema.clone();

        for row in &mut self.rows {
            let should_update = if let Some(wc) = where_clause {
                Self::row_matches_static(&schema, row, wc)
            } else {
                true
            };

            if should_update {
                for (col_name, value) in assignments {
                    if let Some(col_idx) = schema.column_index(col_name) {
                        row.values[col_idx] = value.clone();
                    }
                }
                updated_count += 1;
            }
        }

        // Rebuild index if primary key was updated
        let pk_updated = assignments.iter().any(|(name, _)| {
            self.schema
                .column(name)
                .map(|c| c.primary_key)
                .unwrap_or(false)
        });

        if pk_updated {
            self.rebuild_index();
        }

        Ok(updated_count)
    }

    /// Delete rows matching a WHERE clause
    pub fn delete(&mut self, where_clause: Option<&WhereClause>) -> Result<usize, String> {
        let original_len = self.rows.len();

        // Clone schema to avoid borrow issues
        let schema = self.schema.clone();

        self.rows.retain(|row| {
            if let Some(wc) = where_clause {
                !Self::row_matches_static(&schema, row, wc)
            } else {
                false // Delete all if no WHERE
            }
        });

        let deleted_count = original_len - self.rows.len();
        
        // Rebuild index
        self.rebuild_index();

        Ok(deleted_count)
    }
}

/// Result of executing a SQL statement
#[derive(Debug)]
pub enum ExecutionResult {
    /// SELECT result
    Select(ResultSet),
    /// Number of rows affected (INSERT, UPDATE, DELETE)
    RowsAffected(usize),
    /// Table created
    TableCreated(String),
    /// Table dropped
    TableDropped(String),
    /// List of tables
    Tables(Vec<String>),
    /// Table description
    TableDescription {
        table_name: String,
        columns: Vec<ColumnDef>,
    },
    /// Error message
    Error(String),
}

impl ExecutionResult {
    pub fn to_string(&self) -> String {
        match self {
            ExecutionResult::Select(rs) => rs.to_table_string(),
            ExecutionResult::RowsAffected(n) => format!("{} row(s) affected", n),
            ExecutionResult::TableCreated(name) => format!("Table '{}' created", name),
            ExecutionResult::TableDropped(name) => format!("Table '{}' dropped", name),
            ExecutionResult::Tables(tables) => {
                if tables.is_empty() {
                    "No tables".to_string()
                } else {
                    tables.join("\n")
                }
            }
            ExecutionResult::TableDescription { table_name, columns } => {
                let mut result = format!("Table: {}\n", table_name);
                result.push_str("+----------------+----------+----------+-------------+\n");
                result.push_str("| Column         | Type     | Nullable | Primary Key |\n");
                result.push_str("+----------------+----------+----------+-------------+\n");
                for col in columns {
                    result.push_str(&format!(
                        "| {:14} | {:8} | {:8} | {:11} |\n",
                        col.name,
                        col.data_type.to_string(),
                        if col.nullable { "YES" } else { "NO" },
                        if col.primary_key { "YES" } else { "NO" }
                    ));
                }
                result.push_str("+----------------+----------+----------+-------------+\n");
                result
            }
            ExecutionResult::Error(msg) => format!("Error: {}", msg),
        }
    }
}

/// The query executor
pub struct Executor {
    tables: Arc<DashMap<String, Table>>,
}

impl Executor {
    pub fn new() -> Self {
        Executor {
            tables: Arc::new(DashMap::new()),
        }
    }

    /// Create executor with pre-existing tables (for persistence)
    pub fn with_tables(tables: HashMap<String, Table>) -> Self {
        let dashmap = DashMap::new();
        for (name, table) in tables {
            dashmap.insert(name, table);
        }
        Executor {
            tables: Arc::new(dashmap),
        }
    }

    /// Get all tables (for persistence)
    pub fn get_tables(&self) -> HashMap<String, Table> {
        self.tables
            .iter()
            .map(|entry| (entry.key().clone(), entry.value().clone()))
            .collect()
    }

    /// Execute a parsed SQL statement
    pub fn execute(&self, statement: Statement) -> ExecutionResult {
        match statement {
            Statement::CreateTable {
                table_name,
                columns,
                if_not_exists,
            } => self.execute_create_table(&table_name, columns, if_not_exists),

            Statement::DropTable {
                table_name,
                if_exists,
            } => self.execute_drop_table(&table_name, if_exists),

            Statement::Insert {
                table_name,
                columns,
                values,
            } => self.execute_insert(&table_name, columns, values),

            Statement::Select {
                columns,
                table_name,
                where_clause,
                order_by,
                limit,
            } => self.execute_select(&table_name, columns, where_clause, order_by, limit),

            Statement::Update {
                table_name,
                assignments,
                where_clause,
            } => self.execute_update(&table_name, assignments, where_clause),

            Statement::Delete {
                table_name,
                where_clause,
            } => self.execute_delete(&table_name, where_clause),

            Statement::ShowTables => self.execute_show_tables(),

            Statement::DescribeTable { table_name } => self.execute_describe(&table_name),
        }
    }

    fn execute_create_table(
        &self,
        name: &str,
        columns: Vec<ColumnDef>,
        if_not_exists: bool,
    ) -> ExecutionResult {
        let name_lower = name.to_lowercase();
        
        if self.tables.contains_key(&name_lower) {
            if if_not_exists {
                return ExecutionResult::TableCreated(name.to_string());
            }
            return ExecutionResult::Error(format!("Table '{}' already exists", name));
        }

        let schema = TableSchema::new(&name_lower, columns);
        let table = Table::new(schema);
        self.tables.insert(name_lower, table);

        ExecutionResult::TableCreated(name.to_string())
    }

    fn execute_drop_table(&self, name: &str, if_exists: bool) -> ExecutionResult {
        let name_lower = name.to_lowercase();
        
        if self.tables.remove(&name_lower).is_some() {
            ExecutionResult::TableDropped(name.to_string())
        } else if if_exists {
            ExecutionResult::TableDropped(name.to_string())
        } else {
            ExecutionResult::Error(format!("Table '{}' does not exist", name))
        }
    }

    fn execute_insert(
        &self,
        table_name: &str,
        columns: Option<Vec<String>>,
        values: Vec<Vec<Value>>,
    ) -> ExecutionResult {
        let table_name_lower = table_name.to_lowercase();
        
        let mut table = match self.tables.get_mut(&table_name_lower) {
            Some(t) => t,
            None => return ExecutionResult::Error(format!("Table '{}' does not exist", table_name)),
        };

        let mut inserted = 0;

        for row_values in values {
            // If columns are specified, reorder values to match schema
            let final_values = if let Some(ref cols) = columns {
                let mut ordered = vec![Value::Null; table.schema.columns.len()];
                
                if cols.len() != row_values.len() {
                    return ExecutionResult::Error(format!(
                        "Column count ({}) doesn't match value count ({})",
                        cols.len(),
                        row_values.len()
                    ));
                }

                for (i, col_name) in cols.iter().enumerate() {
                    if let Some(idx) = table.schema.column_index(col_name) {
                        ordered[idx] = row_values[i].clone();
                    } else {
                        return ExecutionResult::Error(format!("Unknown column: {}", col_name));
                    }
                }

                // Apply defaults for missing columns
                for (i, col) in table.schema.columns.iter().enumerate() {
                    if matches!(ordered[i], Value::Null) {
                        if let Some(ref default) = col.default {
                            ordered[i] = default.clone();
                        } else if !col.nullable && !col.primary_key {
                            return ExecutionResult::Error(format!(
                                "Column '{}' requires a value",
                                col.name
                            ));
                        }
                    }
                }

                ordered
            } else {
                row_values
            };

            let row = Row::new(final_values);
            match table.insert(row) {
                Ok(()) => inserted += 1,
                Err(e) => return ExecutionResult::Error(e),
            }
        }

        ExecutionResult::RowsAffected(inserted)
    }

    fn execute_select(
        &self,
        table_name: &str,
        columns: SelectColumns,
        where_clause: Option<WhereClause>,
        order_by: Option<OrderBy>,
        limit: Option<usize>,
    ) -> ExecutionResult {
        let table_name_lower = table_name.to_lowercase();
        
        let table = match self.tables.get(&table_name_lower) {
            Some(t) => t,
            None => return ExecutionResult::Error(format!("Table '{}' does not exist", table_name)),
        };

        match table.select(&columns, where_clause.as_ref(), order_by.as_ref(), limit) {
            Ok(result) => ExecutionResult::Select(result),
            Err(e) => ExecutionResult::Error(e),
        }
    }

    fn execute_update(
        &self,
        table_name: &str,
        assignments: Vec<(String, Value)>,
        where_clause: Option<WhereClause>,
    ) -> ExecutionResult {
        let table_name_lower = table_name.to_lowercase();
        
        let mut table = match self.tables.get_mut(&table_name_lower) {
            Some(t) => t,
            None => return ExecutionResult::Error(format!("Table '{}' does not exist", table_name)),
        };

        match table.update(&assignments, where_clause.as_ref()) {
            Ok(count) => ExecutionResult::RowsAffected(count),
            Err(e) => ExecutionResult::Error(e),
        }
    }

    fn execute_delete(
        &self,
        table_name: &str,
        where_clause: Option<WhereClause>,
    ) -> ExecutionResult {
        let table_name_lower = table_name.to_lowercase();
        
        let mut table = match self.tables.get_mut(&table_name_lower) {
            Some(t) => t,
            None => return ExecutionResult::Error(format!("Table '{}' does not exist", table_name)),
        };

        match table.delete(where_clause.as_ref()) {
            Ok(count) => ExecutionResult::RowsAffected(count),
            Err(e) => ExecutionResult::Error(e),
        }
    }

    fn execute_show_tables(&self) -> ExecutionResult {
        let tables: Vec<String> = self
            .tables
            .iter()
            .map(|entry| entry.key().clone())
            .collect();
        ExecutionResult::Tables(tables)
    }

    fn execute_describe(&self, table_name: &str) -> ExecutionResult {
        let table_name_lower = table_name.to_lowercase();
        
        let table = match self.tables.get(&table_name_lower) {
            Some(t) => t,
            None => return ExecutionResult::Error(format!("Table '{}' does not exist", table_name)),
        };

        ExecutionResult::TableDescription {
            table_name: table.schema.name.clone(),
            columns: table.schema.columns.clone(),
        }
    }
}

impl Default for Executor {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for Executor {
    fn clone(&self) -> Self {
        Executor {
            tables: Arc::clone(&self.tables),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_and_insert() {
        let executor = Executor::new();
        
        // Create table
        let stmt = parse_sql("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)").unwrap();
        let result = executor.execute(stmt);
        assert!(matches!(result, ExecutionResult::TableCreated(_)));

        // Insert row
        let stmt = parse_sql("INSERT INTO users VALUES (1, 'Alice')").unwrap();
        let result = executor.execute(stmt);
        assert!(matches!(result, ExecutionResult::RowsAffected(1)));

        // Select all
        let stmt = parse_sql("SELECT * FROM users").unwrap();
        if let ExecutionResult::Select(rs) = executor.execute(stmt) {
            assert_eq!(rs.row_count(), 1);
        } else {
            panic!("Expected Select result");
        }
    }

    #[test]
    fn test_select_with_where() {
        let executor = Executor::new();
        
        executor.execute(parse_sql("CREATE TABLE items (id INTEGER, name TEXT, price INTEGER)").unwrap());
        executor.execute(parse_sql("INSERT INTO items VALUES (1, 'Apple', 100)").unwrap());
        executor.execute(parse_sql("INSERT INTO items VALUES (2, 'Banana', 50)").unwrap());
        executor.execute(parse_sql("INSERT INTO items VALUES (3, 'Cherry', 200)").unwrap());

        // Select with WHERE
        let stmt = parse_sql("SELECT name FROM items WHERE price > 75").unwrap();
        if let ExecutionResult::Select(rs) = executor.execute(stmt) {
            assert_eq!(rs.row_count(), 2);
        } else {
            panic!("Expected Select result");
        }
    }

    #[test]
    fn test_update() {
        let executor = Executor::new();
        
        executor.execute(parse_sql("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)").unwrap());
        executor.execute(parse_sql("INSERT INTO users VALUES (1, 'Alice')").unwrap());

        let stmt = parse_sql("UPDATE users SET name = 'Bob' WHERE id = 1").unwrap();
        let result = executor.execute(stmt);
        assert!(matches!(result, ExecutionResult::RowsAffected(1)));

        let stmt = parse_sql("SELECT name FROM users WHERE id = 1").unwrap();
        if let ExecutionResult::Select(rs) = executor.execute(stmt) {
            assert_eq!(rs.rows[0].values[0], Value::Text("Bob".to_string()));
        }
    }

    #[test]
    fn test_delete() {
        let executor = Executor::new();
        
        executor.execute(parse_sql("CREATE TABLE users (id INTEGER, name TEXT)").unwrap());
        executor.execute(parse_sql("INSERT INTO users VALUES (1, 'Alice')").unwrap());
        executor.execute(parse_sql("INSERT INTO users VALUES (2, 'Bob')").unwrap());

        let stmt = parse_sql("DELETE FROM users WHERE id = 1").unwrap();
        let result = executor.execute(stmt);
        assert!(matches!(result, ExecutionResult::RowsAffected(1)));

        let stmt = parse_sql("SELECT * FROM users").unwrap();
        if let ExecutionResult::Select(rs) = executor.execute(stmt) {
            assert_eq!(rs.row_count(), 1);
        }
    }
}
