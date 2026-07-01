use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, Mutex, RwLock};

use super::parser::Statement;
use super::planner::Planner;
use super::types::*;

const DEFAULT_BLOOM_FALSE_POSITIVE_RATE: f64 = 0.01;

fn default_bloom_false_positive_rate() -> f64 {
    DEFAULT_BLOOM_FALSE_POSITIVE_RATE
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
enum KeyPart {
    Null,
    Boolean(bool),
    Integer(i64),
    Float(u64),
    Text(String),
}

impl KeyPart {
    fn from_value(value: &Value) -> Self {
        match value {
            Value::Null => Self::Null,
            Value::Boolean(value) => Self::Boolean(*value),
            Value::Integer(value) => Self::Integer(*value),
            Value::Float(value) => {
                let bits = value.to_bits();
                let ordered = if bits & (1 << 63) != 0 {
                    !bits
                } else {
                    bits ^ (1 << 63)
                };
                Self::Float(ordered)
            }
            Value::Text(value) => Self::Text(value.clone()),
        }
    }
}

type IndexKey = Vec<KeyPart>;

#[derive(Debug, Clone)]
struct BloomFilter {
    bits: Vec<u64>,
    hashes: usize,
}

impl BloomFilter {
    fn new(expected_items: usize, false_positive_rate: f64) -> Self {
        let expected = expected_items.max(1) as f64;
        let rate = false_positive_rate.clamp(0.000_001, 0.5);
        let bit_count = (-(expected * rate.ln()) / (std::f64::consts::LN_2.powi(2)))
            .ceil()
            .max(64.0) as usize;
        let hashes = ((bit_count as f64 / expected) * std::f64::consts::LN_2)
            .round()
            .clamp(1.0, 16.0) as usize;
        Self {
            bits: vec![0; bit_count.div_ceil(64)],
            hashes,
        }
    }

    fn positions(&self, key: &IndexKey) -> impl Iterator<Item = usize> + '_ {
        let text = format!("{key:?}");
        let first = fnv1a(text.as_bytes(), 0xcbf29ce484222325);
        let second = fnv1a(text.as_bytes(), 0x84222325cbf29ce4) | 1;
        let bit_count = self.bits.len() * 64;
        (0..self.hashes).map(move |index| {
            first.wrapping_add((index as u64).wrapping_mul(second)) as usize % bit_count
        })
    }

    fn insert(&mut self, key: &IndexKey) {
        let positions: Vec<usize> = self.positions(key).collect();
        for position in positions {
            self.bits[position / 64] |= 1u64 << (position % 64);
        }
    }

    fn might_contain(&self, key: &IndexKey) -> bool {
        self.positions(key)
            .all(|position| self.bits[position / 64] & (1u64 << (position % 64)) != 0)
    }
}

fn fnv1a(bytes: &[u8], seed: u64) -> u64 {
    bytes.iter().fold(seed, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    })
}

#[derive(Debug, Clone)]
struct BTreeIndex {
    definition: IndexDef,
    entries: BTreeMap<IndexKey, BTreeSet<u64>>,
    bloom: BloomFilter,
}

impl BTreeIndex {
    fn empty(definition: IndexDef, expected_items: usize, false_positive_rate: f64) -> Self {
        Self {
            definition,
            entries: BTreeMap::new(),
            bloom: BloomFilter::new(expected_items, false_positive_rate),
        }
    }
}

/// A table with stable row IDs and runtime B-tree indexes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Table {
    pub schema: TableSchema,
    pub rows: Vec<Row>,
    #[serde(default)]
    pub row_ids: Vec<u64>,
    #[serde(default = "default_next_row_id")]
    pub next_row_id: u64,
    /// Compatibility map retained for callers of the original primary-key API.
    #[serde(skip)]
    pub primary_key_index: HashMap<String, usize>,
    #[serde(skip)]
    indexes: HashMap<String, BTreeIndex>,
    #[serde(skip, default = "default_bloom_false_positive_rate")]
    bloom_false_positive_rate: f64,
}

fn default_next_row_id() -> u64 {
    1
}

impl Table {
    pub fn new(schema: TableSchema) -> Self {
        let mut table = Self {
            schema,
            rows: Vec::new(),
            row_ids: Vec::new(),
            next_row_id: 1,
            primary_key_index: HashMap::new(),
            indexes: HashMap::new(),
            bloom_false_positive_rate: DEFAULT_BLOOM_FALSE_POSITIVE_RATE,
        };
        table.rebuild_index();
        table
    }

    pub fn migrate_legacy_rows(&mut self) {
        if self.schema.constraints.is_empty() {
            for column in &self.schema.columns {
                if column.primary_key {
                    self.schema.constraints.push(TableConstraint::PrimaryKey {
                        name: None,
                        columns: vec![column.name.clone()],
                    });
                } else if column.unique {
                    self.schema.constraints.push(TableConstraint::Unique {
                        name: None,
                        columns: vec![column.name.clone()],
                    });
                }
            }
        }
        if self.schema.indexes.is_empty() {
            self.schema.indexes = automatic_indexes(&self.schema);
        }
        if self.row_ids.len() != self.rows.len() {
            self.row_ids = (1..=self.rows.len() as u64).collect();
        }
        self.next_row_id = self
            .row_ids
            .iter()
            .copied()
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        self.rebuild_index();
    }

    pub fn rebuild_index(&mut self) {
        self.primary_key_index.clear();
        if let Some(primary_index) = self.schema.primary_key_index() {
            for (position, row) in self.rows.iter().enumerate() {
                if let Some(value) = row.values.get(primary_index) {
                    self.primary_key_index.insert(value.to_string(), position);
                }
            }
        }
        self.indexes.clear();
        for definition in self.schema.indexes.clone() {
            let mut index = BTreeIndex::empty(
                definition.clone(),
                self.rows.len(),
                self.bloom_false_positive_rate,
            );
            for (position, row) in self.rows.iter().enumerate() {
                let row_id = self
                    .row_ids
                    .get(position)
                    .copied()
                    .unwrap_or(position as u64 + 1);
                if let Some(key) = self.index_key(row, &definition.columns) {
                    index.entries.entry(key.clone()).or_default().insert(row_id);
                    index.bloom.insert(&key);
                }
            }
            self.indexes.insert(definition.name.clone(), index);
        }
    }

    pub fn set_bloom_false_positive_rate(&mut self, rate: f64) {
        self.bloom_false_positive_rate = rate.clamp(0.000_001, 0.5);
        self.rebuild_index();
    }

    fn index_key(&self, row: &Row, columns: &[String]) -> Option<IndexKey> {
        columns
            .iter()
            .map(|column| {
                self.schema
                    .column_index(column)
                    .and_then(|index| row.values.get(index))
                    .map(KeyPart::from_value)
            })
            .collect()
    }

    fn insert_rows(&mut self, rows: Vec<Row>) -> Result<usize, String> {
        for row in &rows {
            self.schema.validate_row(&row.values)?;
        }
        let count = rows.len();
        for row in rows {
            self.rows.push(row);
            self.row_ids.push(self.next_row_id);
            self.next_row_id = self.next_row_id.saturating_add(1);
        }
        self.rebuild_index();
        Ok(count)
    }

    fn candidate_row_ids(
        &self,
        selection: Option<&Expr>,
    ) -> (Option<BTreeSet<u64>>, Option<String>) {
        let Some(selection) = selection else {
            return (None, None);
        };
        let predicates = collect_index_predicates(selection);
        let mut best: Option<(BTreeSet<u64>, String, usize)> = None;
        for index in self.indexes.values() {
            let mut prefix = Vec::new();
            let mut range: Option<(ComparisonOp, KeyPart)> = None;
            for column in &index.definition.columns {
                if let Some((operator, value)) = predicates
                    .iter()
                    .find(|predicate| predicate.column.eq_ignore_ascii_case(column))
                    .map(|predicate| (predicate.operator.clone(), predicate.value.clone()))
                {
                    if operator == ComparisonOp::Equal && range.is_none() {
                        prefix.push(KeyPart::from_value(&value));
                    } else if range.is_none()
                        && matches!(
                            operator,
                            ComparisonOp::LessThan
                                | ComparisonOp::LessThanOrEqual
                                | ComparisonOp::GreaterThan
                                | ComparisonOp::GreaterThanOrEqual
                        )
                    {
                        range = Some((operator, KeyPart::from_value(&value)));
                        break;
                    } else {
                        break;
                    }
                } else {
                    break;
                }
            }
            if prefix.is_empty() && range.is_none() {
                continue;
            }
            let mut ids = BTreeSet::new();
            if prefix.len() == index.definition.columns.len() && range.is_none() {
                if index.bloom.might_contain(&prefix)
                    && let Some(found) = index.entries.get(&prefix)
                {
                    ids.extend(found);
                }
            } else {
                for (key, found) in &index.entries {
                    if !key.starts_with(&prefix) {
                        continue;
                    }
                    let range_matches = match &range {
                        Some((operator, value)) => key
                            .get(prefix.len())
                            .is_some_and(|part| compare_key_part(part, value, operator)),
                        None => true,
                    };
                    if range_matches {
                        ids.extend(found);
                    }
                }
            }
            let score = prefix.len() * 2 + usize::from(range.is_some());
            if best.as_ref().is_none_or(|(_, _, current)| score > *current) {
                best = Some((ids, index.definition.name.clone(), score));
            }
        }
        best.map(|(ids, name, _)| (Some(ids), Some(name)))
            .unwrap_or((None, None))
    }

    pub(crate) fn best_index_name(&self, selection: Option<&Expr>) -> Option<String> {
        self.candidate_row_ids(selection).1
    }
}

fn compare_key_part(left: &KeyPart, right: &KeyPart, operator: &ComparisonOp) -> bool {
    let ordering = left.cmp(right);
    match operator {
        ComparisonOp::Equal => ordering == Ordering::Equal,
        ComparisonOp::NotEqual => ordering != Ordering::Equal,
        ComparisonOp::LessThan => ordering == Ordering::Less,
        ComparisonOp::LessThanOrEqual => ordering != Ordering::Greater,
        ComparisonOp::GreaterThan => ordering == Ordering::Greater,
        ComparisonOp::GreaterThanOrEqual => ordering != Ordering::Less,
        ComparisonOp::Like => false,
    }
}

#[derive(Clone)]
struct IndexPredicate {
    column: String,
    operator: ComparisonOp,
    value: Value,
}

fn collect_index_predicates(expression: &Expr) -> Vec<IndexPredicate> {
    match expression {
        Expr::Binary {
            left,
            op: BinaryOp::And,
            right,
        } => {
            let mut predicates = collect_index_predicates(left);
            predicates.extend(collect_index_predicates(right));
            predicates
        }
        Expr::Binary {
            left,
            op: BinaryOp::Compare(operator),
            right,
        } => match (&**left, &**right) {
            (Expr::Column { name, .. }, Expr::Literal(value)) => vec![IndexPredicate {
                column: name.clone(),
                operator: operator.clone(),
                value: value.clone(),
            }],
            (Expr::Literal(value), Expr::Column { name, .. }) => vec![IndexPredicate {
                column: name.clone(),
                operator: reverse_comparison(operator),
                value: value.clone(),
            }],
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

fn reverse_comparison(operator: &ComparisonOp) -> ComparisonOp {
    match operator {
        ComparisonOp::LessThan => ComparisonOp::GreaterThan,
        ComparisonOp::LessThanOrEqual => ComparisonOp::GreaterThanOrEqual,
        ComparisonOp::GreaterThan => ComparisonOp::LessThan,
        ComparisonOp::GreaterThanOrEqual => ComparisonOp::LessThanOrEqual,
        other => other.clone(),
    }
}

#[derive(Debug, Clone, Default)]
pub struct Catalog {
    pub version: u64,
    pub schema_version: u64,
    pub tables: HashMap<String, Arc<Table>>,
    scratch_pool: Arc<ScratchPool>,
    subquery_cache: Arc<Mutex<HashMap<String, CachedSubquery>>>,
}

#[derive(Debug)]
struct ScratchPool {
    capacity: usize,
    buffers: Mutex<Vec<Vec<Value>>>,
}

impl Default for ScratchPool {
    fn default() -> Self {
        Self {
            capacity: 32,
            buffers: Mutex::new(Vec::new()),
        }
    }
}

impl ScratchPool {
    fn acquire(&self) -> Vec<Value> {
        self.buffers
            .lock()
            .expect("scratch pool lock poisoned")
            .pop()
            .unwrap_or_default()
    }

    fn release(&self, mut buffer: Vec<Value>) {
        buffer.clear();
        let mut buffers = self.buffers.lock().expect("scratch pool lock poisoned");
        if buffers.len() < self.capacity {
            buffers.push(buffer);
        }
    }
}

impl Catalog {
    pub fn from_tables(tables: HashMap<String, Table>) -> Self {
        Self {
            version: 0,
            schema_version: 0,
            tables: tables
                .into_iter()
                .map(|(name, mut table)| {
                    table.migrate_legacy_rows();
                    (name.to_ascii_lowercase(), Arc::new(table))
                })
                .collect(),
            scratch_pool: Arc::new(ScratchPool::default()),
            subquery_cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn owned_tables(&self) -> HashMap<String, Table> {
        self.tables
            .iter()
            .map(|(name, table)| (name.clone(), (**table).clone()))
            .collect()
    }

    pub fn configure_bloom_false_positive_rate(&mut self, rate: f64) {
        let names: Vec<String> = self.tables.keys().cloned().collect();
        for name in names {
            if let Some(table) = self.tables.get(&name) {
                let mut table = (**table).clone();
                table.set_bloom_false_positive_rate(rate);
                self.tables.insert(name, Arc::new(table));
            }
        }
    }

    pub fn configure_memory_pool_capacity(&mut self, capacity: usize) {
        self.scratch_pool = Arc::new(ScratchPool {
            capacity,
            buffers: Mutex::new(Vec::with_capacity(capacity)),
        });
    }
}

#[derive(Debug)]
pub enum ExecutionResult {
    Select(ResultSet),
    RowsAffected(usize),
    TableCreated(String),
    TableDropped(String),
    IndexCreated(String),
    IndexDropped(String),
    Tables(Vec<String>),
    Indexes(Vec<IndexDef>),
    TableDescription {
        table_name: String,
        columns: Vec<ColumnDef>,
    },
    Explain(Vec<String>),
    TransactionStarted,
    TransactionCommitted,
    TransactionRolledBack,
    Error(String),
}

impl fmt::Display for ExecutionResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let output = match self {
            Self::Select(result) => result.to_table_string(),
            Self::RowsAffected(count) => format!("{count} row(s) affected"),
            Self::TableCreated(name) => format!("Table '{name}' created"),
            Self::TableDropped(name) => format!("Table '{name}' dropped"),
            Self::IndexCreated(name) => format!("Index '{name}' created"),
            Self::IndexDropped(name) => format!("Index '{name}' dropped"),
            Self::Tables(tables) => {
                if tables.is_empty() {
                    "No tables".into()
                } else {
                    tables.join("\n")
                }
            }
            Self::Indexes(indexes) => indexes
                .iter()
                .map(|index| {
                    format!(
                        "{} ({}){}",
                        index.name,
                        index.columns.join(", "),
                        if index.unique { " UNIQUE" } else { "" }
                    )
                })
                .collect::<Vec<_>>()
                .join("\n"),
            Self::TableDescription {
                table_name,
                columns,
            } => {
                let mut output = format!("Table: {table_name}\n");
                output
                    .push_str("+----------------+----------+----------+-------------+--------+\n");
                output
                    .push_str("| Column         | Type     | Nullable | Primary Key | Unique |\n");
                output
                    .push_str("+----------------+----------+----------+-------------+--------+\n");
                for column in columns {
                    output.push_str(&format!(
                        "| {:14} | {:8} | {:8} | {:11} | {:6} |\n",
                        column.name,
                        column.data_type,
                        if column.nullable { "YES" } else { "NO" },
                        if column.primary_key { "YES" } else { "NO" },
                        if column.unique { "YES" } else { "NO" }
                    ));
                }
                output
                    .push_str("+----------------+----------+----------+-------------+--------+\n");
                output
            }
            Self::Explain(lines) => lines.join("\n"),
            Self::TransactionStarted => "Transaction started".into(),
            Self::TransactionCommitted => "Transaction committed".into(),
            Self::TransactionRolledBack => "Transaction rolled back".into(),
            Self::Error(message) => format!("Error: {message}"),
        };
        f.write_str(&output)
    }
}

#[derive(Clone)]
pub struct Executor {
    catalog: Arc<RwLock<Catalog>>,
}

impl Executor {
    pub fn new() -> Self {
        Self {
            catalog: Arc::new(RwLock::new(Catalog::default())),
        }
    }

    pub fn with_tables(tables: HashMap<String, Table>) -> Self {
        Self {
            catalog: Arc::new(RwLock::new(Catalog::from_tables(tables))),
        }
    }

    pub fn from_catalog(catalog: Catalog) -> Self {
        Self {
            catalog: Arc::new(RwLock::new(catalog)),
        }
    }

    pub fn snapshot(&self) -> Catalog {
        self.catalog.read().expect("catalog lock poisoned").clone()
    }

    pub fn catalog_handle(&self) -> Arc<RwLock<Catalog>> {
        Arc::clone(&self.catalog)
    }

    pub fn configure_bloom_false_positive_rate(&self, rate: f64) {
        let mut catalog = self.catalog.write().expect("catalog lock poisoned");
        catalog.configure_bloom_false_positive_rate(rate);
    }

    pub fn configure_memory_pool_capacity(&self, capacity: usize) {
        self.catalog
            .write()
            .expect("catalog lock poisoned")
            .configure_memory_pool_capacity(capacity);
    }

    pub fn get_tables(&self) -> HashMap<String, Table> {
        self.snapshot().owned_tables()
    }

    pub fn execute(&self, statement: Statement) -> ExecutionResult {
        if is_transaction_control(&statement) {
            return ExecutionResult::Error(
                "Transaction control requires SQLDatabase::session()".to_string(),
            );
        }
        if is_write_statement(&statement) {
            let mut guard = self.catalog.write().expect("catalog lock poisoned");
            let mut working = guard.clone();
            let result = Self::execute_catalog(&mut working, statement);
            if !matches!(result, ExecutionResult::Error(_)) {
                working.version = guard.version.saturating_add(1);
                *guard = working;
            }
            result
        } else {
            let mut snapshot = self.snapshot();
            execute_statement(&mut snapshot, statement)
        }
    }

    pub(crate) fn execute_catalog(catalog: &mut Catalog, statement: Statement) -> ExecutionResult {
        if is_write_statement(&statement) {
            let mut working = catalog.clone();
            let result = execute_statement(&mut working, statement);
            if matches!(result, ExecutionResult::Error(_)) {
                return result;
            }
            if let Err(error) = validate_catalog(&working) {
                return ExecutionResult::Error(error);
            }
            *catalog = working;
            result
        } else {
            execute_statement(catalog, statement)
        }
    }
}

impl Default for Executor {
    fn default() -> Self {
        Self::new()
    }
}

pub(crate) fn is_write_statement(statement: &Statement) -> bool {
    matches!(
        statement,
        Statement::CreateTable { .. }
            | Statement::DropTable { .. }
            | Statement::CreateIndex { .. }
            | Statement::DropIndex { .. }
            | Statement::Insert { .. }
            | Statement::Update { .. }
            | Statement::Delete { .. }
    )
}

pub(crate) fn is_transaction_control(statement: &Statement) -> bool {
    matches!(
        statement,
        Statement::Begin | Statement::Commit | Statement::Rollback
    )
}

fn execute_statement(catalog: &mut Catalog, statement: Statement) -> ExecutionResult {
    catalog
        .subquery_cache
        .lock()
        .expect("subquery cache lock poisoned")
        .clear();
    match statement {
        Statement::CreateTable {
            table_name,
            columns,
            constraints,
            if_not_exists,
        } => execute_create_table(catalog, table_name, columns, constraints, if_not_exists),
        Statement::DropTable {
            table_name,
            if_exists,
        } => execute_drop_table(catalog, &table_name, if_exists),
        Statement::CreateIndex {
            name,
            table_name,
            columns,
            unique,
            if_not_exists,
        } => execute_create_index(catalog, name, table_name, columns, unique, if_not_exists),
        Statement::DropIndex {
            name,
            table_name,
            if_exists,
        } => execute_drop_index(catalog, &name, &table_name, if_exists),
        Statement::Insert {
            table_name,
            columns,
            values,
        } => execute_insert(catalog, &table_name, columns, values),
        Statement::Query(query) => execute_query(catalog, &query, &HashMap::new())
            .map(relation_to_result)
            .map(ExecutionResult::Select)
            .unwrap_or_else(ExecutionResult::Error),
        Statement::Update {
            table_name,
            assignments,
            selection,
        } => execute_update(catalog, &table_name, assignments, selection.as_ref()),
        Statement::Delete {
            table_name,
            selection,
        } => execute_delete(catalog, &table_name, selection.as_ref()),
        Statement::ShowTables => {
            let mut tables: Vec<String> = catalog.tables.keys().cloned().collect();
            tables.sort();
            ExecutionResult::Tables(tables)
        }
        Statement::ShowIndexes { table_name } => catalog
            .tables
            .get(&table_name.to_ascii_lowercase())
            .map(|table| ExecutionResult::Indexes(table.schema.indexes.clone()))
            .unwrap_or_else(|| {
                ExecutionResult::Error(format!("Table '{table_name}' does not exist"))
            }),
        Statement::DescribeTable { table_name } => catalog
            .tables
            .get(&table_name.to_ascii_lowercase())
            .map(|table| ExecutionResult::TableDescription {
                table_name: table.schema.name.clone(),
                columns: table.schema.columns.clone(),
            })
            .unwrap_or_else(|| {
                ExecutionResult::Error(format!("Table '{table_name}' does not exist"))
            }),
        Statement::Explain(statement) => {
            ExecutionResult::Explain(explain_statement(catalog, &statement))
        }
        Statement::Begin | Statement::Commit | Statement::Rollback => {
            ExecutionResult::Error("Transaction control requires a session".to_string())
        }
    }
}

fn execute_create_table(
    catalog: &mut Catalog,
    table_name: String,
    mut columns: Vec<ColumnDef>,
    constraints: Vec<TableConstraint>,
    if_not_exists: bool,
) -> ExecutionResult {
    let name = table_name.to_ascii_lowercase();
    if catalog.tables.contains_key(&name) {
        return if if_not_exists {
            ExecutionResult::TableCreated(name)
        } else {
            ExecutionResult::Error(format!("Table '{name}' already exists"))
        };
    }
    let mut seen = HashSet::new();
    for column in &columns {
        if !seen.insert(column.name.to_ascii_lowercase()) {
            return ExecutionResult::Error(format!("Duplicate column '{}'", column.name));
        }
    }
    let mut primary_keys = 0;
    for constraint in &constraints {
        let constrained = match constraint {
            TableConstraint::PrimaryKey {
                columns: key_columns,
                ..
            } => {
                primary_keys += 1;
                for key_column in key_columns {
                    if let Some(column) = columns
                        .iter_mut()
                        .find(|column| column.name.eq_ignore_ascii_case(key_column))
                    {
                        column.nullable = false;
                        column.unique = key_columns.len() == 1;
                        column.primary_key = key_columns.len() == 1;
                    }
                }
                key_columns
            }
            TableConstraint::Unique { columns, .. } => columns,
            TableConstraint::ForeignKey(foreign_key) => &foreign_key.columns,
            TableConstraint::Check { .. } => continue,
        };
        for column in constrained {
            if !seen.contains(&column.to_ascii_lowercase()) {
                return ExecutionResult::Error(format!("Unknown constrained column '{column}'"));
            }
        }
    }
    if primary_keys > 1 {
        return ExecutionResult::Error("A table may have only one PRIMARY KEY".to_string());
    }
    let mut schema = TableSchema::new(&name, columns);
    schema.constraints = constraints;
    schema.indexes = automatic_indexes(&schema);
    catalog
        .tables
        .insert(name.clone(), Arc::new(Table::new(schema)));
    ExecutionResult::TableCreated(name)
}

fn automatic_indexes(schema: &TableSchema) -> Vec<IndexDef> {
    let mut indexes = Vec::new();
    for constraint in &schema.constraints {
        match constraint {
            TableConstraint::PrimaryKey { name, columns } => indexes.push(IndexDef {
                name: name
                    .clone()
                    .unwrap_or_else(|| format!("{}_pk", schema.name)),
                columns: columns.clone(),
                unique: true,
                automatic: true,
            }),
            TableConstraint::Unique { name, columns } => indexes.push(IndexDef {
                name: name
                    .clone()
                    .unwrap_or_else(|| format!("{}_{}_uk", schema.name, columns.join("_"))),
                columns: columns.clone(),
                unique: true,
                automatic: true,
            }),
            TableConstraint::ForeignKey(foreign_key) => indexes.push(IndexDef {
                name: foreign_key.name.clone().unwrap_or_else(|| {
                    format!("{}_{}_fk", schema.name, foreign_key.columns.join("_"))
                }),
                columns: foreign_key.columns.clone(),
                unique: false,
                automatic: true,
            }),
            TableConstraint::Check { .. } => {}
        }
    }
    for column in &schema.columns {
        if column.primary_key
            && !indexes
                .iter()
                .any(|index| index.columns == vec![column.name.clone()])
        {
            indexes.push(IndexDef {
                name: format!("{}_{}_pk", schema.name, column.name),
                columns: vec![column.name.clone()],
                unique: true,
                automatic: true,
            });
        } else if column.unique
            && !indexes
                .iter()
                .any(|index| index.columns == vec![column.name.clone()])
        {
            indexes.push(IndexDef {
                name: format!("{}_{}_uk", schema.name, column.name),
                columns: vec![column.name.clone()],
                unique: true,
                automatic: true,
            });
        }
    }
    indexes
}

fn execute_drop_table(catalog: &mut Catalog, table_name: &str, if_exists: bool) -> ExecutionResult {
    let name = table_name.to_ascii_lowercase();
    if !catalog.tables.contains_key(&name) {
        return if if_exists {
            ExecutionResult::TableDropped(name)
        } else {
            ExecutionResult::Error(format!("Table '{name}' does not exist"))
        };
    }
    for (other_name, table) in &catalog.tables {
        if other_name == &name {
            continue;
        }
        if table.schema.constraints.iter().any(|constraint| {
            matches!(
                constraint,
                TableConstraint::ForeignKey(foreign_key)
                    if foreign_key.foreign_table.eq_ignore_ascii_case(&name)
            )
        }) {
            return ExecutionResult::Error(format!(
                "Cannot drop table '{name}': referenced by table '{other_name}'"
            ));
        }
    }
    catalog.tables.remove(&name);
    ExecutionResult::TableDropped(name)
}

fn execute_create_index(
    catalog: &mut Catalog,
    name: String,
    table_name: String,
    columns: Vec<String>,
    unique: bool,
    if_not_exists: bool,
) -> ExecutionResult {
    let table_name = table_name.to_ascii_lowercase();
    let Some(existing) = catalog.tables.get(&table_name) else {
        return ExecutionResult::Error(format!("Table '{table_name}' does not exist"));
    };
    if existing
        .schema
        .indexes
        .iter()
        .any(|index| index.name.eq_ignore_ascii_case(&name))
    {
        return if if_not_exists {
            ExecutionResult::IndexCreated(name)
        } else {
            ExecutionResult::Error(format!("Index '{name}' already exists"))
        };
    }
    if columns.is_empty() {
        return ExecutionResult::Error("An index requires at least one column".to_string());
    }
    for column in &columns {
        if existing.schema.column_index(column).is_none() {
            return ExecutionResult::Error(format!("Unknown column '{column}'"));
        }
    }
    let mut table = (**existing).clone();
    table.schema.indexes.push(IndexDef {
        name: name.to_ascii_lowercase(),
        columns,
        unique,
        automatic: false,
    });
    table.rebuild_index();
    if let Err(error) = validate_unique_indexes(&table) {
        return ExecutionResult::Error(error);
    }
    catalog.tables.insert(table_name, Arc::new(table));
    ExecutionResult::IndexCreated(name)
}

fn execute_drop_index(
    catalog: &mut Catalog,
    name: &str,
    table_name: &str,
    if_exists: bool,
) -> ExecutionResult {
    let table_name = table_name.to_ascii_lowercase();
    let Some(existing) = catalog.tables.get(&table_name) else {
        return ExecutionResult::Error(format!("Table '{table_name}' does not exist"));
    };
    let Some(position) = existing
        .schema
        .indexes
        .iter()
        .position(|index| index.name.eq_ignore_ascii_case(name))
    else {
        return if if_exists {
            ExecutionResult::IndexDropped(name.to_string())
        } else {
            ExecutionResult::Error(format!("Index '{name}' does not exist"))
        };
    };
    if existing.schema.indexes[position].automatic {
        return ExecutionResult::Error(format!(
            "Cannot drop index '{name}': required by a constraint"
        ));
    }
    let mut table = (**existing).clone();
    table.schema.indexes.remove(position);
    table.rebuild_index();
    catalog.tables.insert(table_name, Arc::new(table));
    ExecutionResult::IndexDropped(name.to_string())
}

fn execute_insert(
    catalog: &mut Catalog,
    table_name: &str,
    columns: Option<Vec<String>>,
    values: Vec<Vec<Expr>>,
) -> ExecutionResult {
    let name = table_name.to_ascii_lowercase();
    let Some(existing) = catalog.tables.get(&name) else {
        return ExecutionResult::Error(format!("Table '{name}' does not exist"));
    };
    let mut table = (**existing).clone();
    let mut rows = Vec::with_capacity(values.len());
    for expressions in values {
        let raw = match expressions
            .iter()
            .map(eval_constant)
            .collect::<Result<Vec<_>, _>>()
        {
            Ok(values) => values,
            Err(error) => return ExecutionResult::Error(error),
        };
        let mut ordered = vec![Value::Null; table.schema.columns.len()];
        if let Some(columns) = &columns {
            if columns.len() != raw.len() {
                return ExecutionResult::Error(format!(
                    "Column count ({}) doesn't match value count ({})",
                    columns.len(),
                    raw.len()
                ));
            }
            let mut used = HashSet::new();
            for (column, value) in columns.iter().zip(raw) {
                let Some(index) = table.schema.column_index(column) else {
                    return ExecutionResult::Error(format!("Unknown column '{column}'"));
                };
                if !used.insert(index) {
                    return ExecutionResult::Error(format!("Duplicate column '{column}'"));
                }
                ordered[index] = value;
            }
        } else {
            if raw.len() != ordered.len() {
                return ExecutionResult::Error(format!(
                    "Expected {} values, got {}",
                    ordered.len(),
                    raw.len()
                ));
            }
            ordered = raw;
        }
        for (index, column) in table.schema.columns.iter().enumerate() {
            if ordered[index].is_null()
                && let Some(default) = &column.default
            {
                ordered[index] = default.clone();
            }
            if let Some(coerced) = ordered[index].coerce_to(&column.data_type) {
                ordered[index] = coerced;
            }
        }
        rows.push(Row::new(ordered));
    }
    let count = match table.insert_rows(rows) {
        Ok(count) => count,
        Err(error) => return ExecutionResult::Error(error),
    };
    catalog.tables.insert(name, Arc::new(table));
    match validate_catalog(catalog) {
        Ok(()) => ExecutionResult::RowsAffected(count),
        Err(error) => ExecutionResult::Error(error),
    }
}

fn execute_update(
    catalog: &mut Catalog,
    table_name: &str,
    assignments: Vec<(String, Expr)>,
    selection: Option<&Expr>,
) -> ExecutionResult {
    let name = table_name.to_ascii_lowercase();
    let Some(existing) = catalog.tables.get(&name) else {
        return ExecutionResult::Error(format!("Table '{name}' does not exist"));
    };
    let mut table = (**existing).clone();
    let relation = relation_from_table(&table, &name, None, selection);
    let (candidate_ids, _) = table.candidate_row_ids(selection);
    let mut updated = 0;
    for position in 0..table.rows.len() {
        if candidate_ids.as_ref().is_some_and(|ids| {
            table
                .row_ids
                .get(position)
                .is_none_or(|row_id| !ids.contains(row_id))
        }) {
            continue;
        }
        let values = table.rows[position].values.clone();
        let matches = match selection {
            Some(expression) => match eval_expr(
                expression,
                &relation.columns,
                &values,
                None,
                catalog,
                &HashMap::new(),
            ) {
                Ok(value) => value.truth() == SqlTruth::True,
                Err(error) => return ExecutionResult::Error(error),
            },
            None => true,
        };
        if !matches {
            continue;
        }
        let original = table.rows[position].values.clone();
        for (column, expression) in &assignments {
            let Some(column_index) = table.schema.column_index(column) else {
                return ExecutionResult::Error(format!("Unknown column '{column}'"));
            };
            let value = match eval_expr(
                expression,
                &relation.columns,
                &original,
                None,
                catalog,
                &HashMap::new(),
            ) {
                Ok(value) => value,
                Err(error) => return ExecutionResult::Error(error),
            };
            let Some(coerced) = value.coerce_to(&table.schema.columns[column_index].data_type)
            else {
                return ExecutionResult::Error(format!("Type mismatch for column '{column}'"));
            };
            table.rows[position].values[column_index] = coerced;
        }
        updated += 1;
    }
    table.rebuild_index();
    catalog.tables.insert(name, Arc::new(table));
    match validate_catalog(catalog) {
        Ok(()) => ExecutionResult::RowsAffected(updated),
        Err(error) => ExecutionResult::Error(error),
    }
}

fn execute_delete(
    catalog: &mut Catalog,
    table_name: &str,
    selection: Option<&Expr>,
) -> ExecutionResult {
    let name = table_name.to_ascii_lowercase();
    let Some(existing) = catalog.tables.get(&name) else {
        return ExecutionResult::Error(format!("Table '{name}' does not exist"));
    };
    let mut table = (**existing).clone();
    let relation = relation_from_table(&table, &name, None, selection);
    let (candidate_ids, _) = table.candidate_row_ids(selection);
    let mut kept_rows = Vec::new();
    let mut kept_ids = Vec::new();
    let mut deleted = 0;
    for (position, row) in table.rows.iter().enumerate() {
        if candidate_ids.as_ref().is_some_and(|ids| {
            table
                .row_ids
                .get(position)
                .is_none_or(|row_id| !ids.contains(row_id))
        }) {
            kept_rows.push(row.clone());
            kept_ids.push(table.row_ids[position]);
            continue;
        }
        let should_delete = match selection {
            Some(expression) => match eval_expr(
                expression,
                &relation.columns,
                &row.values,
                None,
                catalog,
                &HashMap::new(),
            ) {
                Ok(value) => value.truth() == SqlTruth::True,
                Err(error) => return ExecutionResult::Error(error),
            },
            None => true,
        };
        if should_delete {
            deleted += 1;
        } else {
            kept_rows.push(row.clone());
            kept_ids.push(table.row_ids[position]);
        }
    }
    table.rows = kept_rows;
    table.row_ids = kept_ids;
    table.rebuild_index();
    catalog.tables.insert(name, Arc::new(table));
    match validate_catalog(catalog) {
        Ok(()) => ExecutionResult::RowsAffected(deleted),
        Err(error) => ExecutionResult::Error(error),
    }
}

#[derive(Debug, Clone)]
struct BoundColumn {
    table: String,
    name: String,
}

#[derive(Debug, Clone, Default)]
struct Relation {
    columns: Vec<BoundColumn>,
    rows: Vec<Vec<Value>>,
}

#[derive(Debug, Clone)]
struct CachedSubquery {
    relation: Arc<Relation>,
    first_column_keys: HashSet<String>,
    first_column_has_null: bool,
}

fn relation_to_result(relation: Relation) -> ResultSet {
    ResultSet {
        columns: relation
            .columns
            .into_iter()
            .map(|column| column.name)
            .collect(),
        rows: relation.rows.into_iter().map(Row::new).collect(),
    }
}

fn execute_query(
    catalog: &Catalog,
    query: &Query,
    inherited_ctes: &HashMap<String, Relation>,
) -> Result<Relation, String> {
    let mut ctes = inherited_ctes.clone();
    for cte in &query.ctes {
        let mut relation = execute_query(catalog, &cte.query, &ctes)?;
        if !cte.columns.is_empty() {
            if cte.columns.len() != relation.columns.len() {
                return Err(format!("CTE '{}' column count does not match", cte.name));
            }
            for (column, name) in relation.columns.iter_mut().zip(&cte.columns) {
                column.name = name.clone();
            }
        }
        for column in &mut relation.columns {
            column.table = cte.name.clone();
        }
        ctes.insert(cte.name.clone(), relation);
    }

    let mut relation = match &query.from {
        Some(source) => source_relation(catalog, source, &ctes, query.selection.as_ref())?,
        None => Relation {
            columns: Vec::new(),
            rows: vec![Vec::new()],
        },
    };

    for join in &query.joins {
        let right = source_relation(catalog, &join.source, &ctes, None)?;
        relation = join_relations(catalog, relation, right, join, &ctes)?;
    }

    if let Some(selection) = &query.selection {
        let mut filtered = Vec::new();
        for row in relation.rows {
            if eval_expr(selection, &relation.columns, &row, None, catalog, &ctes)?.truth()
                == SqlTruth::True
            {
                filtered.push(row);
            }
        }
        relation.rows = filtered;
    }

    let aggregate = !query.group_by.is_empty()
        || query.projection.iter().any(select_item_has_aggregate)
        || query.having.as_ref().is_some_and(expr_has_aggregate);

    let order_before_projection = !aggregate
        && !query.order_by.is_empty()
        && query
            .order_by
            .iter()
            .all(|order| expression_resolves(&order.expr, &relation.columns));
    if order_before_projection {
        let columns = relation.columns.clone();
        relation.rows.sort_by(|left, right| {
            compare_order_rows(left, right, &columns, &query.order_by, catalog, &ctes)
                .unwrap_or(Ordering::Equal)
        });
    }

    let mut projected = if aggregate {
        aggregate_query(catalog, query, relation, &ctes)?
    } else {
        project_query(catalog, query, relation, &ctes)?
    };

    if query.distinct {
        let mut seen = HashSet::new();
        projected.rows.retain(|row| seen.insert(row_key(row)));
    }

    if !query.order_by.is_empty() && !order_before_projection {
        let columns = projected.columns.clone();
        let compare = |left: &Vec<Value>, right: &Vec<Value>| {
            compare_order_rows(left, right, &columns, &query.order_by, catalog, &ctes)
                .unwrap_or(Ordering::Equal)
        };
        if let Some(limit) = query.limit {
            if limit == 0 {
                projected.rows.clear();
            } else if limit < projected.rows.len() {
                projected.rows.select_nth_unstable_by(limit, compare);
                projected.rows.truncate(limit);
                projected.rows.sort_by(compare);
            } else {
                projected.rows.sort_by(compare);
            }
        } else {
            projected.rows.sort_by(compare);
        }
    }
    if let Some(limit) = query.limit {
        projected.rows.truncate(limit);
    }
    Ok(projected)
}

fn expression_resolves(expression: &Expr, columns: &[BoundColumn]) -> bool {
    match expression {
        Expr::Column { qualifier, name } => {
            resolve_column(columns, qualifier.as_deref(), name).is_ok()
        }
        Expr::Literal(_) => true,
        Expr::Binary { left, right, .. } => {
            expression_resolves(left, columns) && expression_resolves(right, columns)
        }
        Expr::Unary { expr, .. } | Expr::IsNull { expr, .. } | Expr::Like { expr, .. } => {
            expression_resolves(expr, columns)
        }
        Expr::InList { expr, list, .. } => {
            expression_resolves(expr, columns)
                && list.iter().all(|item| expression_resolves(item, columns))
        }
        Expr::Aggregate { .. }
        | Expr::InSubquery { .. }
        | Expr::Exists { .. }
        | Expr::ScalarSubquery(_) => false,
    }
}

fn source_relation(
    catalog: &Catalog,
    source: &TableSource,
    ctes: &HashMap<String, Relation>,
    selection: Option<&Expr>,
) -> Result<Relation, String> {
    match source {
        TableSource::Table { name, alias } => {
            let visible_name = alias.as_deref().unwrap_or(name);
            if let Some(cte) = ctes.get(name) {
                let mut relation = cte.clone();
                for column in &mut relation.columns {
                    column.table = visible_name.to_string();
                }
                return Ok(relation);
            }
            let table = catalog
                .tables
                .get(name)
                .ok_or_else(|| format!("Table '{name}' does not exist"))?;
            Ok(relation_from_table(
                table,
                visible_name,
                alias.as_deref(),
                selection,
            ))
        }
        TableSource::Derived { query, alias } => {
            let mut relation = execute_query(catalog, query, ctes)?;
            for column in &mut relation.columns {
                column.table = alias.clone();
            }
            Ok(relation)
        }
    }
}

fn relation_from_table(
    table: &Table,
    visible_name: &str,
    _alias: Option<&str>,
    selection: Option<&Expr>,
) -> Relation {
    let (candidate_ids, _) = table.candidate_row_ids(selection);
    Relation {
        columns: table
            .schema
            .columns
            .iter()
            .map(|column| BoundColumn {
                table: visible_name.to_string(),
                name: column.name.clone(),
            })
            .collect(),
        rows: table
            .rows
            .iter()
            .enumerate()
            .filter(|(position, _)| {
                candidate_ids.as_ref().is_none_or(|ids| {
                    table
                        .row_ids
                        .get(*position)
                        .is_some_and(|row_id| ids.contains(row_id))
                })
            })
            .map(|(_, row)| row.values.clone())
            .collect(),
    }
}

fn join_relations(
    catalog: &Catalog,
    left: Relation,
    right: Relation,
    join: &Join,
    ctes: &HashMap<String, Relation>,
) -> Result<Relation, String> {
    let mut columns = left.columns.clone();
    columns.extend(right.columns.clone());
    if let Some((left_index, right_index)) = join
        .on
        .as_ref()
        .and_then(|expression| hash_join_indices(expression, &left.columns, &right.columns))
    {
        return hash_join(
            left,
            right,
            columns,
            join.join_type.clone(),
            left_index,
            right_index,
        );
    }
    let mut rows = Vec::new();
    let mut right_matched = vec![false; right.rows.len()];
    for left_row in &left.rows {
        let mut left_matched = false;
        for (right_position, right_row) in right.rows.iter().enumerate() {
            let mut combined = left_row.clone();
            combined.extend(right_row.clone());
            let matches = match &join.on {
                Some(expression) => {
                    eval_expr(expression, &columns, &combined, None, catalog, ctes)?.truth()
                        == SqlTruth::True
                }
                None => true,
            };
            if matches {
                left_matched = true;
                right_matched[right_position] = true;
                rows.push(combined);
            }
        }
        if !left_matched && join.join_type == JoinType::Left {
            let mut combined = left_row.clone();
            combined.extend(vec![Value::Null; right.columns.len()]);
            rows.push(combined);
        }
    }
    if join.join_type == JoinType::Right {
        for (position, right_row) in right.rows.iter().enumerate() {
            if !right_matched[position] {
                let mut combined = vec![Value::Null; left.columns.len()];
                combined.extend(right_row.clone());
                rows.push(combined);
            }
        }
    }
    Ok(Relation { columns, rows })
}

fn hash_join_indices(
    expression: &Expr,
    left_columns: &[BoundColumn],
    right_columns: &[BoundColumn],
) -> Option<(usize, usize)> {
    let Expr::Binary {
        left,
        op: BinaryOp::Compare(ComparisonOp::Equal),
        right,
    } = expression
    else {
        return None;
    };
    let Expr::Column {
        qualifier: left_qualifier,
        name: left_name,
    } = &**left
    else {
        return None;
    };
    let Expr::Column {
        qualifier: right_qualifier,
        name: right_name,
    } = &**right
    else {
        return None;
    };
    if let (Ok(left_index), Ok(right_index)) = (
        resolve_column(left_columns, left_qualifier.as_deref(), left_name),
        resolve_column(right_columns, right_qualifier.as_deref(), right_name),
    ) {
        return Some((left_index, right_index));
    }
    if let (Ok(left_index), Ok(right_index)) = (
        resolve_column(left_columns, right_qualifier.as_deref(), right_name),
        resolve_column(right_columns, left_qualifier.as_deref(), left_name),
    ) {
        return Some((left_index, right_index));
    }
    None
}

fn hash_join(
    left: Relation,
    right: Relation,
    columns: Vec<BoundColumn>,
    join_type: JoinType,
    left_index: usize,
    right_index: usize,
) -> Result<Relation, String> {
    let mut right_map: HashMap<String, Vec<usize>> = HashMap::new();
    for (position, row) in right.rows.iter().enumerate() {
        if !row[right_index].is_null() {
            right_map
                .entry(hash_join_key(&row[right_index]))
                .or_default()
                .push(position);
        }
    }
    let mut right_matched = vec![false; right.rows.len()];
    let mut rows = Vec::new();
    for left_row in &left.rows {
        let matches = if left_row[left_index].is_null() {
            None
        } else {
            right_map.get(&hash_join_key(&left_row[left_index]))
        };
        let mut matched = false;
        if let Some(positions) = matches {
            for position in positions {
                // Hash keys normalize numeric values, but retain a SQL equality check
                // to protect against collisions and mixed-type keys.
                if left_row[left_index]
                    .compare_sql(&right.rows[*position][right_index], &ComparisonOp::Equal)
                    != SqlTruth::True
                {
                    continue;
                }
                matched = true;
                right_matched[*position] = true;
                let mut combined = left_row.clone();
                combined.extend(right.rows[*position].clone());
                rows.push(combined);
            }
        }
        if !matched && join_type == JoinType::Left {
            let mut combined = left_row.clone();
            combined.extend(vec![Value::Null; right.columns.len()]);
            rows.push(combined);
        }
    }
    if join_type == JoinType::Right {
        for (position, right_row) in right.rows.iter().enumerate() {
            if !right_matched[position] {
                let mut combined = vec![Value::Null; left.columns.len()];
                combined.extend(right_row.clone());
                rows.push(combined);
            }
        }
    }
    Ok(Relation { columns, rows })
}

fn hash_join_key(value: &Value) -> String {
    match value {
        Value::Integer(value) => format!("number:{:016x}", (*value as f64).to_bits()),
        Value::Float(value) => format!("number:{:016x}", value.to_bits()),
        _ => value_key(value),
    }
}

fn project_query(
    catalog: &Catalog,
    query: &Query,
    source: Relation,
    ctes: &HashMap<String, Relation>,
) -> Result<Relation, String> {
    let columns = projection_columns(&query.projection, &source.columns)?;
    let mut rows = Vec::with_capacity(source.rows.len());
    for row in &source.rows {
        rows.push(project_row(
            &query.projection,
            &source.columns,
            row,
            None,
            catalog,
            ctes,
        )?);
    }
    Ok(Relation { columns, rows })
}

fn aggregate_query(
    catalog: &Catalog,
    query: &Query,
    source: Relation,
    ctes: &HashMap<String, Relation>,
) -> Result<Relation, String> {
    let mut groups: BTreeMap<String, Vec<Vec<Value>>> = BTreeMap::new();
    for row in &source.rows {
        let mut key_values = catalog.scratch_pool.acquire();
        for expression in &query.group_by {
            key_values.push(eval_expr(
                expression,
                &source.columns,
                row,
                None,
                catalog,
                ctes,
            )?);
        }
        let key = row_key(&key_values);
        catalog.scratch_pool.release(key_values);
        groups.entry(key).or_default().push(row.clone());
    }
    if groups.is_empty() && query.group_by.is_empty() {
        groups.insert(String::new(), Vec::new());
    }
    let columns = projection_columns(&query.projection, &source.columns)?;
    let mut rows = Vec::new();
    for group in groups.values() {
        let representative = group
            .first()
            .cloned()
            .unwrap_or_else(|| vec![Value::Null; source.columns.len()]);
        if let Some(having) = &query.having
            && eval_expr(
                having,
                &source.columns,
                &representative,
                Some(group),
                catalog,
                ctes,
            )?
            .truth()
                != SqlTruth::True
        {
            continue;
        }
        rows.push(project_row(
            &query.projection,
            &source.columns,
            &representative,
            Some(group),
            catalog,
            ctes,
        )?);
    }
    Ok(Relation { columns, rows })
}

fn projection_columns(
    projection: &[SelectItem],
    source_columns: &[BoundColumn],
) -> Result<Vec<BoundColumn>, String> {
    let mut output = Vec::new();
    for item in projection {
        match item {
            SelectItem::Wildcard(qualifier) => {
                let matching: Vec<_> = source_columns
                    .iter()
                    .filter(|column| {
                        qualifier
                            .as_ref()
                            .is_none_or(|name| column.table.eq_ignore_ascii_case(name))
                    })
                    .cloned()
                    .collect();
                if matching.is_empty() {
                    return Err(format!(
                        "Unknown wildcard qualifier '{}'",
                        qualifier.as_deref().unwrap_or("*")
                    ));
                }
                output.extend(matching);
            }
            SelectItem::Expr { expr, alias } => output.push(BoundColumn {
                table: match expr {
                    Expr::Column {
                        qualifier: Some(qualifier),
                        ..
                    } if alias.is_none() => qualifier.clone(),
                    _ => String::new(),
                },
                name: alias.clone().unwrap_or_else(|| expression_name(expr)),
            }),
        }
    }
    Ok(output)
}

fn project_row(
    projection: &[SelectItem],
    source_columns: &[BoundColumn],
    row: &[Value],
    group: Option<&Vec<Vec<Value>>>,
    catalog: &Catalog,
    ctes: &HashMap<String, Relation>,
) -> Result<Vec<Value>, String> {
    let mut output = Vec::new();
    for item in projection {
        match item {
            SelectItem::Wildcard(qualifier) => {
                for (index, column) in source_columns.iter().enumerate() {
                    if qualifier
                        .as_ref()
                        .is_none_or(|name| column.table.eq_ignore_ascii_case(name))
                    {
                        output.push(row[index].clone());
                    }
                }
            }
            SelectItem::Expr { expr, .. } => {
                output.push(eval_expr(expr, source_columns, row, group, catalog, ctes)?);
            }
        }
    }
    Ok(output)
}

fn expression_name(expression: &Expr) -> String {
    match expression {
        Expr::Column { name, .. } => name.clone(),
        Expr::Aggregate { function, .. } => format!("{function:?}").to_ascii_lowercase(),
        Expr::ScalarSubquery(_) => "subquery".to_string(),
        _ => "expression".to_string(),
    }
}

fn eval_expr(
    expression: &Expr,
    columns: &[BoundColumn],
    row: &[Value],
    group: Option<&Vec<Vec<Value>>>,
    catalog: &Catalog,
    ctes: &HashMap<String, Relation>,
) -> Result<Value, String> {
    match expression {
        Expr::Literal(value) => Ok(value.clone()),
        Expr::Column { qualifier, name } => {
            let index = resolve_column(columns, qualifier.as_deref(), name)?;
            row.get(index)
                .cloned()
                .ok_or_else(|| format!("Column '{name}' is unavailable"))
        }
        Expr::Unary { op, expr } => {
            let value = eval_expr(expr, columns, row, group, catalog, ctes)?;
            match (op, value) {
                (UnaryOp::Not, value) => Ok(truth_value(value.truth().negate())),
                (_, Value::Null) => Ok(Value::Null),
                (UnaryOp::Negate, Value::Integer(value)) => Ok(Value::Integer(-value)),
                (UnaryOp::Negate, Value::Float(value)) => Ok(Value::Float(-value)),
                (UnaryOp::Plus, Value::Integer(value)) => Ok(Value::Integer(value)),
                (UnaryOp::Plus, Value::Float(value)) => Ok(Value::Float(value)),
                _ => Err("Unary operator requires a numeric/boolean value".to_string()),
            }
        }
        Expr::Binary { left, op, right } => {
            if *op == BinaryOp::And {
                let left = eval_expr(left, columns, row, group, catalog, ctes)?.truth();
                if left == SqlTruth::False {
                    return Ok(Value::Boolean(false));
                }
                let right = eval_expr(right, columns, row, group, catalog, ctes)?.truth();
                return Ok(truth_value(left.and(right)));
            }
            if *op == BinaryOp::Or {
                let left = eval_expr(left, columns, row, group, catalog, ctes)?.truth();
                if left == SqlTruth::True {
                    return Ok(Value::Boolean(true));
                }
                let right = eval_expr(right, columns, row, group, catalog, ctes)?.truth();
                return Ok(truth_value(left.or(right)));
            }
            let left = eval_expr(left, columns, row, group, catalog, ctes)?;
            let right = eval_expr(right, columns, row, group, catalog, ctes)?;
            match op {
                BinaryOp::Compare(operator) => Ok(truth_value(left.compare_sql(&right, operator))),
                BinaryOp::Add => numeric_binary(left, right, |a, b| a + b, |a, b| a + b),
                BinaryOp::Subtract => numeric_binary(left, right, |a, b| a - b, |a, b| a - b),
                BinaryOp::Multiply => numeric_binary(left, right, |a, b| a * b, |a, b| a * b),
                BinaryOp::Divide => {
                    if matches!(right, Value::Integer(0) | Value::Float(0.0)) {
                        return Err("Division by zero".to_string());
                    }
                    numeric_binary(left, right, |a, b| a / b, |a, b| a / b)
                }
                BinaryOp::And | BinaryOp::Or => unreachable!(),
            }
        }
        Expr::IsNull { expr, negated } => {
            let is_null = eval_expr(expr, columns, row, group, catalog, ctes)?.is_null();
            Ok(Value::Boolean(if *negated { !is_null } else { is_null }))
        }
        Expr::Like {
            expr,
            pattern,
            negated,
        } => {
            let value = eval_expr(expr, columns, row, group, catalog, ctes)?;
            let pattern = eval_expr(pattern, columns, row, group, catalog, ctes)?;
            let truth = value.compare_sql(&pattern, &ComparisonOp::Like);
            Ok(truth_value(if *negated { truth.negate() } else { truth }))
        }
        Expr::InList {
            expr,
            list,
            negated,
        } => {
            let value = eval_expr(expr, columns, row, group, catalog, ctes)?;
            let values = list
                .iter()
                .map(|item| eval_expr(item, columns, row, group, catalog, ctes))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(truth_value(in_values(&value, &values, *negated)))
        }
        Expr::InSubquery {
            expr,
            query,
            negated,
        } => {
            let value = eval_expr(expr, columns, row, group, catalog, ctes)?;
            let cached = execute_subquery_cached(catalog, query, ctes)?;
            if cached.relation.columns.len() != 1 {
                return Err("IN subquery must return one column".to_string());
            }
            let truth = if value.is_null() {
                SqlTruth::Unknown
            } else if cached.first_column_keys.contains(&hash_join_key(&value)) {
                SqlTruth::True
            } else if cached.first_column_has_null {
                SqlTruth::Unknown
            } else {
                SqlTruth::False
            };
            Ok(truth_value(if *negated { truth.negate() } else { truth }))
        }
        Expr::Exists { query, negated } => {
            let exists = !execute_subquery_cached(catalog, query, ctes)?
                .relation
                .rows
                .is_empty();
            Ok(Value::Boolean(if *negated { !exists } else { exists }))
        }
        Expr::ScalarSubquery(query) => {
            let cached = execute_subquery_cached(catalog, query, ctes)?;
            if cached.relation.columns.len() != 1 {
                return Err("Scalar subquery must return one column".to_string());
            }
            match cached.relation.rows.as_slice() {
                [] => Ok(Value::Null),
                [row] => Ok(row[0].clone()),
                _ => Err("Scalar subquery returned more than one row".to_string()),
            }
        }
        Expr::Aggregate {
            function,
            expr,
            distinct,
        } => {
            let group = group.ok_or_else(|| "Aggregate used outside grouping".to_string())?;
            eval_aggregate(
                function,
                expr.as_deref(),
                *distinct,
                columns,
                group,
                catalog,
                ctes,
            )
        }
    }
}

fn execute_subquery_cached(
    catalog: &Catalog,
    query: &Query,
    ctes: &HashMap<String, Relation>,
) -> Result<CachedSubquery, String> {
    // The cache is cleared at every statement boundary. Pointer identity is
    // therefore a cheap and safe key for one immutable AST and CTE environment.
    let key = format!(
        "{}:{:p}:{:p}",
        catalog.version, query, ctes as *const HashMap<String, Relation>
    );
    if let Some(relation) = catalog
        .subquery_cache
        .lock()
        .expect("subquery cache lock poisoned")
        .get(&key)
        .cloned()
    {
        return Ok(relation);
    }
    let relation = execute_query(catalog, query, ctes)?;
    let mut first_column_keys = HashSet::new();
    let mut first_column_has_null = false;
    for row in &relation.rows {
        if let Some(value) = row.first() {
            if value.is_null() {
                first_column_has_null = true;
            } else {
                first_column_keys.insert(hash_join_key(value));
            }
        }
    }
    let cached = CachedSubquery {
        relation: Arc::new(relation),
        first_column_keys,
        first_column_has_null,
    };
    catalog
        .subquery_cache
        .lock()
        .expect("subquery cache lock poisoned")
        .insert(key, cached.clone());
    Ok(cached)
}

fn eval_aggregate(
    function: &AggregateFunction,
    expression: Option<&Expr>,
    distinct: bool,
    columns: &[BoundColumn],
    group: &[Vec<Value>],
    catalog: &Catalog,
    ctes: &HashMap<String, Relation>,
) -> Result<Value, String> {
    let mut values = if let Some(expression) = expression {
        group
            .iter()
            .map(|row| eval_expr(expression, columns, row, None, catalog, ctes))
            .collect::<Result<Vec<_>, _>>()?
    } else {
        vec![Value::Integer(1); group.len()]
    };
    if distinct {
        let mut seen = HashSet::new();
        values.retain(|value| seen.insert(value_key(value)));
    }
    if *function == AggregateFunction::Count {
        return Ok(Value::Integer(
            values.iter().filter(|value| !value.is_null()).count() as i64,
        ));
    }
    values.retain(|value| !value.is_null());
    if values.is_empty() {
        return Ok(Value::Null);
    }
    match function {
        AggregateFunction::Sum | AggregateFunction::Avg => {
            let mut sum = 0.0;
            let mut all_integer = true;
            for value in &values {
                match value {
                    Value::Integer(value) => sum += *value as f64,
                    Value::Float(value) => {
                        all_integer = false;
                        sum += value;
                    }
                    _ => return Err("SUM/AVG requires numeric values".to_string()),
                }
            }
            if *function == AggregateFunction::Avg {
                Ok(Value::Float(sum / values.len() as f64))
            } else if all_integer {
                Ok(Value::Integer(sum as i64))
            } else {
                Ok(Value::Float(sum))
            }
        }
        AggregateFunction::Min => Ok(values
            .into_iter()
            .min_by(|left, right| left.partial_cmp(right).unwrap_or(Ordering::Equal))
            .unwrap()),
        AggregateFunction::Max => Ok(values
            .into_iter()
            .max_by(|left, right| left.partial_cmp(right).unwrap_or(Ordering::Equal))
            .unwrap()),
        AggregateFunction::Count => unreachable!(),
    }
}

fn resolve_column(
    columns: &[BoundColumn],
    qualifier: Option<&str>,
    name: &str,
) -> Result<usize, String> {
    let matches: Vec<usize> = columns
        .iter()
        .enumerate()
        .filter(|(_, column)| {
            column.name.eq_ignore_ascii_case(name)
                && qualifier.is_none_or(|table| column.table.eq_ignore_ascii_case(table))
        })
        .map(|(index, _)| index)
        .collect();
    match matches.as_slice() {
        [index] => Ok(*index),
        [] => Err(format!(
            "Unknown column '{}{}'",
            qualifier
                .map(|value| format!("{value}."))
                .unwrap_or_default(),
            name
        )),
        _ => Err(format!("Ambiguous column '{name}'")),
    }
}

fn truth_value(truth: SqlTruth) -> Value {
    match truth {
        SqlTruth::True => Value::Boolean(true),
        SqlTruth::False => Value::Boolean(false),
        SqlTruth::Unknown => Value::Null,
    }
}

fn numeric_binary(
    left: Value,
    right: Value,
    integer: impl FnOnce(i64, i64) -> i64,
    float: impl FnOnce(f64, f64) -> f64,
) -> Result<Value, String> {
    match (left, right) {
        (Value::Null, _) | (_, Value::Null) => Ok(Value::Null),
        (Value::Integer(left), Value::Integer(right)) => Ok(Value::Integer(integer(left, right))),
        (Value::Integer(left), Value::Float(right)) => Ok(Value::Float(float(left as f64, right))),
        (Value::Float(left), Value::Integer(right)) => Ok(Value::Float(float(left, right as f64))),
        (Value::Float(left), Value::Float(right)) => Ok(Value::Float(float(left, right))),
        _ => Err("Arithmetic requires numeric values".to_string()),
    }
}

fn in_values(value: &Value, values: &[Value], negated: bool) -> SqlTruth {
    if value.is_null() {
        return SqlTruth::Unknown;
    }
    let mut unknown = false;
    for candidate in values {
        match value.compare_sql(candidate, &ComparisonOp::Equal) {
            SqlTruth::True => {
                return if negated {
                    SqlTruth::False
                } else {
                    SqlTruth::True
                };
            }
            SqlTruth::Unknown => unknown = true,
            SqlTruth::False => {}
        }
    }
    let result = if unknown {
        SqlTruth::Unknown
    } else {
        SqlTruth::False
    };
    if negated { result.negate() } else { result }
}

fn eval_constant(expression: &Expr) -> Result<Value, String> {
    eval_expr(
        expression,
        &[],
        &[],
        None,
        &Catalog::default(),
        &HashMap::new(),
    )
}

fn row_key(row: &[Value]) -> String {
    row.iter().map(value_key).collect::<Vec<_>>().join("\u{1f}")
}

fn value_key(value: &Value) -> String {
    match value {
        Value::Null => "n".to_string(),
        Value::Boolean(value) => format!("b:{value}"),
        Value::Integer(value) => format!("i:{value}"),
        Value::Float(value) => format!("f:{:016x}", value.to_bits()),
        Value::Text(value) => format!("s:{}:{value}", value.len()),
    }
}

fn select_item_has_aggregate(item: &SelectItem) -> bool {
    matches!(item, SelectItem::Expr { expr, .. } if expr_has_aggregate(expr))
}

fn expr_has_aggregate(expression: &Expr) -> bool {
    match expression {
        Expr::Aggregate { .. } => true,
        Expr::Binary { left, right, .. } => expr_has_aggregate(left) || expr_has_aggregate(right),
        Expr::Unary { expr, .. } | Expr::IsNull { expr, .. } | Expr::Like { expr, .. } => {
            expr_has_aggregate(expr)
        }
        Expr::InList { expr, list, .. } => {
            expr_has_aggregate(expr) || list.iter().any(expr_has_aggregate)
        }
        _ => false,
    }
}

fn compare_order_rows(
    left: &[Value],
    right: &[Value],
    columns: &[BoundColumn],
    order_by: &[OrderBy],
    catalog: &Catalog,
    ctes: &HashMap<String, Relation>,
) -> Result<Ordering, String> {
    for order in order_by {
        let left_value = eval_order_value(&order.expr, columns, left, catalog, ctes)?;
        let right_value = eval_order_value(&order.expr, columns, right, catalog, ctes)?;
        let mut ordering = match (&left_value, &right_value) {
            (Value::Null, Value::Null) => Ordering::Equal,
            // Match MySQL's default ordering: NULL sorts first for ASC and
            // last for DESC (the direction reversal is applied below).
            (Value::Null, _) => Ordering::Less,
            (_, Value::Null) => Ordering::Greater,
            _ => left_value
                .partial_cmp(&right_value)
                .unwrap_or(Ordering::Equal),
        };
        if order.descending {
            ordering = ordering.reverse();
        }
        if ordering != Ordering::Equal {
            return Ok(ordering);
        }
    }
    Ok(Ordering::Equal)
}

fn eval_order_value(
    expression: &Expr,
    columns: &[BoundColumn],
    row: &[Value],
    catalog: &Catalog,
    ctes: &HashMap<String, Relation>,
) -> Result<Value, String> {
    if let Expr::Aggregate { .. } = expression {
        let name = expression_name(expression);
        let index = resolve_column(columns, None, &name)?;
        return row
            .get(index)
            .cloned()
            .ok_or_else(|| format!("ORDER BY expression '{name}' is unavailable"));
    }
    eval_expr(expression, columns, row, None, catalog, ctes)
}

fn validate_catalog(catalog: &Catalog) -> Result<(), String> {
    for table in catalog.tables.values() {
        for row in &table.rows {
            table.schema.validate_row(&row.values)?;
            validate_checks(catalog, table, row)?;
        }
        validate_unique_indexes(table)?;
    }
    validate_foreign_keys(catalog)
}

fn validate_checks(catalog: &Catalog, table: &Table, row: &Row) -> Result<(), String> {
    let columns: Vec<BoundColumn> = table
        .schema
        .columns
        .iter()
        .map(|column| BoundColumn {
            table: table.schema.name.clone(),
            name: column.name.clone(),
        })
        .collect();
    for constraint in &table.schema.constraints {
        if let TableConstraint::Check { name, expr } = constraint {
            let result = eval_expr(expr, &columns, &row.values, None, catalog, &HashMap::new())?;
            // SQL CHECK accepts TRUE or UNKNOWN and rejects FALSE.
            if result.truth() == SqlTruth::False {
                return Err(format!(
                    "CHECK constraint '{}' failed",
                    name.as_deref().unwrap_or("<unnamed>")
                ));
            }
        }
    }
    Ok(())
}

fn validate_unique_indexes(table: &Table) -> Result<(), String> {
    for index in table
        .indexes
        .values()
        .filter(|index| index.definition.unique)
    {
        for (key, row_ids) in &index.entries {
            // UNIQUE permits multiple NULL-containing keys; PRIMARY KEY columns are NOT NULL.
            if !key.contains(&KeyPart::Null) && row_ids.len() > 1 {
                return Err(format!(
                    "UNIQUE constraint '{}' failed",
                    index.definition.name
                ));
            }
        }
    }
    Ok(())
}

fn validate_foreign_keys(catalog: &Catalog) -> Result<(), String> {
    for table in catalog.tables.values() {
        for constraint in &table.schema.constraints {
            let TableConstraint::ForeignKey(foreign_key) = constraint else {
                continue;
            };
            let parent = catalog
                .tables
                .get(&foreign_key.foreign_table)
                .ok_or_else(|| {
                    format!(
                        "Foreign key references missing table '{}'",
                        foreign_key.foreign_table
                    )
                })?;
            let parent_is_unique = parent.schema.indexes.iter().any(|index| {
                index.unique
                    && index
                        .columns
                        .iter()
                        .map(|column| column.to_ascii_lowercase())
                        .collect::<Vec<_>>()
                        == foreign_key
                            .referred_columns
                            .iter()
                            .map(|column| column.to_ascii_lowercase())
                            .collect::<Vec<_>>()
            });
            if !parent_is_unique {
                return Err(format!(
                    "Foreign key target {}({}) is not PRIMARY KEY or UNIQUE",
                    foreign_key.foreign_table,
                    foreign_key.referred_columns.join(", ")
                ));
            }
            let child_indices = foreign_key
                .columns
                .iter()
                .map(|column| {
                    table
                        .schema
                        .column_index(column)
                        .ok_or_else(|| format!("Unknown foreign-key column '{column}'"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            let parent_indices = foreign_key
                .referred_columns
                .iter()
                .map(|column| {
                    parent
                        .schema
                        .column_index(column)
                        .ok_or_else(|| format!("Unknown referenced column '{column}'"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            for child_row in &table.rows {
                let key: Vec<Value> = child_indices
                    .iter()
                    .map(|index| child_row.values[*index].clone())
                    .collect();
                if key.iter().any(Value::is_null) {
                    continue;
                }
                let found = parent.rows.iter().any(|row| {
                    parent_indices
                        .iter()
                        .enumerate()
                        .all(|(key_index, parent_index)| {
                            row.values[*parent_index] == key[key_index]
                        })
                });
                if !found {
                    return Err(format!(
                        "FOREIGN KEY constraint '{}' failed",
                        foreign_key.name.as_deref().unwrap_or("<unnamed>")
                    ));
                }
            }
        }
    }
    Ok(())
}

fn explain_statement(catalog: &Catalog, statement: &Statement) -> Vec<String> {
    match statement {
        Statement::Query(query) => explain_query(catalog, query),
        Statement::Update {
            table_name,
            selection,
            ..
        }
        | Statement::Delete {
            table_name,
            selection,
        } => explain_scan(catalog, table_name, selection.as_ref()),
        Statement::Insert { table_name, .. } => vec![format!("Insert(table={table_name})")],
        Statement::CreateIndex {
            name,
            table_name,
            columns,
            ..
        } => vec![format!(
            "CreateBTreeIndex(name={name}, table={table_name}, columns={})",
            columns.join(",")
        )],
        other => vec![format!("Utility({other:?})")],
    }
}

fn explain_query(catalog: &Catalog, query: &Query) -> Vec<String> {
    Planner::new(catalog).plan(query).explain_lines()
}

fn explain_scan(catalog: &Catalog, table_name: &str, selection: Option<&Expr>) -> Vec<String> {
    match catalog.tables.get(&table_name.to_ascii_lowercase()) {
        Some(table) => {
            let (_, index) = table.candidate_row_ids(selection);
            match index {
                Some(index) => vec![format!(
                    "IndexScan(table={}, index={}, bloom=true)",
                    table_name, index
                )],
                None => vec![format!("SeqScan(table={table_name})")],
            }
        }
        None => vec![format!("MissingTable({table_name})")],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sql::parse_sql;

    fn execute(executor: &Executor, sql: &str) -> ExecutionResult {
        executor.execute(parse_sql(sql).unwrap())
    }

    #[test]
    fn crud_and_index_scan() {
        let executor = Executor::new();
        execute(
            &executor,
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT UNIQUE, age INTEGER)",
        );
        execute(
            &executor,
            "INSERT INTO users VALUES (1, 'Alice', 30), (2, 'Bob', 20)",
        );
        match execute(&executor, "SELECT name FROM users WHERE id = 1") {
            ExecutionResult::Select(result) => {
                assert_eq!(result.rows[0].values[0], Value::Text("Alice".into()));
            }
            result => panic!("unexpected result: {result:?}"),
        }
        match execute(&executor, "EXPLAIN SELECT * FROM users WHERE id = 2") {
            ExecutionResult::Explain(plan) => assert!(plan.join("\n").contains("IndexScan")),
            result => panic!("unexpected result: {result:?}"),
        }
    }

    #[test]
    fn joins_aggregates_and_ctes() {
        let executor = Executor::new();
        execute(
            &executor,
            "CREATE TABLE users (id INT PRIMARY KEY, name TEXT)",
        );
        execute(
            &executor,
            "CREATE TABLE orders (id INT PRIMARY KEY, user_id INT, amount FLOAT)",
        );
        execute(&executor, "INSERT INTO users VALUES (1, 'A'), (2, 'B')");
        execute(
            &executor,
            "INSERT INTO orders VALUES (1, 1, 10), (2, 1, 20)",
        );
        let result = execute(
            &executor,
            "WITH totals AS (
                SELECT user_id, SUM(amount) AS total FROM orders GROUP BY user_id
            )
            SELECT users.name, totals.total
            FROM users LEFT JOIN totals ON users.id = totals.user_id
            ORDER BY users.name",
        );
        match result {
            ExecutionResult::Select(result) => {
                assert_eq!(result.row_count(), 2);
                assert_eq!(result.rows[0].values[1], Value::Float(30.0));
                assert_eq!(result.rows[1].values[1], Value::Null);
            }
            result => panic!("unexpected result: {result:?}"),
        }
        let result = execute(
            &executor,
            "WITH totals AS (
                SELECT user_id, SUM(amount) AS total FROM orders GROUP BY user_id
            )
            SELECT users.id, totals.total
            FROM users LEFT JOIN totals ON users.id = totals.user_id
            ORDER BY totals.total DESC
            LIMIT 1",
        );
        match result {
            ExecutionResult::Select(result) => {
                assert_eq!(result.rows[0].values[0], Value::Integer(1));
                assert_eq!(result.rows[0].values[1], Value::Float(30.0));
            }
            result => panic!("unexpected result: {result:?}"),
        }
    }

    #[test]
    fn constraints_are_statement_atomic() {
        let executor = Executor::new();
        execute(&executor, "CREATE TABLE parent (id INT PRIMARY KEY)");
        execute(
            &executor,
            "CREATE TABLE child (
                id INT PRIMARY KEY,
                parent_id INT,
                score INT CHECK (score >= 0),
                FOREIGN KEY (parent_id) REFERENCES parent(id)
            )",
        );
        execute(&executor, "INSERT INTO parent VALUES (1)");
        assert!(matches!(
            execute(&executor, "INSERT INTO child VALUES (1, 1, 2), (2, 99, 3)"),
            ExecutionResult::Error(_)
        ));
        match execute(&executor, "SELECT COUNT(*) AS count FROM child") {
            ExecutionResult::Select(result) => {
                assert_eq!(result.rows[0].values[0], Value::Integer(0));
            }
            result => panic!("unexpected result: {result:?}"),
        }
    }

    #[test]
    fn right_join_and_derived_subquery() {
        let executor = Executor::new();
        execute(
            &executor,
            "CREATE TABLE left_side (id INT PRIMARY KEY, name TEXT)",
        );
        execute(
            &executor,
            "CREATE TABLE right_side (id INT PRIMARY KEY, left_id INT)",
        );
        execute(&executor, "INSERT INTO left_side VALUES (1, 'one')");
        execute(&executor, "INSERT INTO right_side VALUES (10, 1), (20, 99)");
        match execute(
            &executor,
            "SELECT l.name, r.id
             FROM (SELECT * FROM left_side) l
             RIGHT JOIN right_side r ON l.id = r.left_id
             ORDER BY r.id",
        ) {
            ExecutionResult::Select(result) => {
                assert_eq!(result.row_count(), 2);
                assert_eq!(result.rows[0].values[0], Value::Text("one".into()));
                assert_eq!(result.rows[1].values[0], Value::Null);
            }
            result => panic!("unexpected result: {result:?}"),
        }
    }

    #[test]
    fn aggregates_having_and_null_semantics() {
        let executor = Executor::new();
        execute(&executor, "CREATE TABLE scores (team TEXT, score INT)");
        execute(
            &executor,
            "INSERT INTO scores VALUES ('a', 10), ('a', 20), ('b', NULL)",
        );
        match execute(
            &executor,
            "SELECT team, COUNT(score) AS count, SUM(score) AS total,
                    AVG(score) AS average, MIN(score) AS minimum, MAX(score) AS maximum
             FROM scores GROUP BY team HAVING COUNT(*) > 1",
        ) {
            ExecutionResult::Select(result) => {
                assert_eq!(result.row_count(), 1);
                assert_eq!(result.rows[0].values[1], Value::Integer(2));
                assert_eq!(result.rows[0].values[2], Value::Integer(30));
                assert_eq!(result.rows[0].values[3], Value::Float(15.0));
            }
            result => panic!("unexpected result: {result:?}"),
        }
        match execute(
            &executor,
            "SELECT team, COUNT(*) FROM scores GROUP BY team ORDER BY COUNT(*) DESC",
        ) {
            ExecutionResult::Select(result) => {
                assert_eq!(result.rows[0].values[0], Value::Text("a".into()));
            }
            result => panic!("unexpected result: {result:?}"),
        }
        match execute(
            &executor,
            "SELECT COUNT(score), SUM(score) FROM scores WHERE team = 'z'",
        ) {
            ExecutionResult::Select(result) => {
                assert_eq!(result.rows[0].values[0], Value::Integer(0));
                assert_eq!(result.rows[0].values[1], Value::Null);
            }
            result => panic!("unexpected result: {result:?}"),
        }
        match execute(&executor, "SELECT * FROM scores WHERE score = NULL") {
            ExecutionResult::Select(result) => assert_eq!(result.row_count(), 0),
            result => panic!("unexpected result: {result:?}"),
        }
    }

    #[test]
    fn uncorrelated_subquery_forms() {
        let executor = Executor::new();
        execute(&executor, "CREATE TABLE numbers (value INT PRIMARY KEY)");
        execute(&executor, "INSERT INTO numbers VALUES (1), (2), (3)");
        match execute(
            &executor,
            "SELECT value, (SELECT MAX(value) FROM numbers) AS maximum
             FROM numbers
             WHERE value IN (SELECT value FROM numbers WHERE value > 1)
               AND EXISTS (SELECT value FROM numbers WHERE value = 3)
             ORDER BY value",
        ) {
            ExecutionResult::Select(result) => {
                assert_eq!(result.row_count(), 2);
                assert_eq!(
                    result.rows[0].values,
                    vec![Value::Integer(2), Value::Integer(3)]
                );
                assert_eq!(
                    result.rows[1].values,
                    vec![Value::Integer(3), Value::Integer(3)]
                );
            }
            result => panic!("unexpected result: {result:?}"),
        }
    }

    #[test]
    fn composite_index_range_and_lifecycle() {
        let executor = Executor::new();
        execute(
            &executor,
            "CREATE TABLE events (kind TEXT, sequence INT, payload TEXT)",
        );
        execute(
            &executor,
            "INSERT INTO events VALUES
             ('a', 1, 'one'), ('a', 2, 'two'), ('a', 3, 'three'), ('b', 2, 'other')",
        );
        execute(
            &executor,
            "CREATE INDEX events_kind_sequence ON events(kind, sequence)",
        );
        match execute(
            &executor,
            "EXPLAIN SELECT payload FROM events WHERE kind = 'a' AND sequence >= 2",
        ) {
            ExecutionResult::Explain(plan) => {
                assert!(plan.join("\n").contains("events_kind_sequence"));
            }
            result => panic!("unexpected result: {result:?}"),
        }
        match execute(
            &executor,
            "SELECT payload FROM events WHERE kind = 'a' AND sequence >= 2 ORDER BY sequence",
        ) {
            ExecutionResult::Select(result) => assert_eq!(result.row_count(), 2),
            result => panic!("unexpected result: {result:?}"),
        }
        assert!(matches!(
            execute(&executor, "DROP INDEX events_kind_sequence ON events"),
            ExecutionResult::IndexDropped(_)
        ));
    }

    #[test]
    fn foreign_key_unique_check_and_restrict() {
        let executor = Executor::new();
        execute(
            &executor,
            "CREATE TABLE parents (code TEXT UNIQUE, enabled BOOLEAN)",
        );
        execute(
            &executor,
            "CREATE TABLE children (
                id INT PRIMARY KEY,
                parent_code TEXT,
                quantity INT CHECK (quantity > 0),
                FOREIGN KEY (parent_code) REFERENCES parents(code)
            )",
        );
        execute(&executor, "INSERT INTO parents VALUES ('p', TRUE)");
        assert!(matches!(
            execute(&executor, "INSERT INTO children VALUES (1, 'p', 1)"),
            ExecutionResult::RowsAffected(1)
        ));
        assert!(matches!(
            execute(&executor, "INSERT INTO children VALUES (2, 'p', 0)"),
            ExecutionResult::Error(_)
        ));
        assert!(matches!(
            execute(&executor, "DELETE FROM parents WHERE code = 'p'"),
            ExecutionResult::Error(_)
        ));
        assert!(matches!(
            execute(&executor, "INSERT INTO parents VALUES ('p', FALSE)"),
            ExecutionResult::Error(_)
        ));
    }
}
