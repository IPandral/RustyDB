use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::fmt;

/// Supported SQL data types.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DataType {
    Integer,
    Float,
    Text,
    Boolean,
    Null,
}

impl fmt::Display for DataType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Self::Integer => "INTEGER",
                Self::Float => "FLOAT",
                Self::Text => "TEXT",
                Self::Boolean => "BOOLEAN",
                Self::Null => "NULL",
            }
        )
    }
}

/// A scalar SQL value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Value {
    Integer(i64),
    Float(f64),
    Text(String),
    Boolean(bool),
    Null,
}

impl Value {
    pub fn data_type(&self) -> DataType {
        match self {
            Self::Integer(_) => DataType::Integer,
            Self::Float(_) => DataType::Float,
            Self::Text(_) => DataType::Text,
            Self::Boolean(_) => DataType::Boolean,
            Self::Null => DataType::Null,
        }
    }

    pub fn is_compatible_with(&self, data_type: &DataType) -> bool {
        matches!(
            (self, data_type),
            (Self::Null, _)
                | (Self::Integer(_), DataType::Integer | DataType::Float)
                | (Self::Float(_), DataType::Float)
                | (Self::Text(_), DataType::Text)
                | (Self::Boolean(_), DataType::Boolean)
        )
    }

    pub fn coerce_to(&self, data_type: &DataType) -> Option<Self> {
        match (self, data_type) {
            (Self::Null, _) => Some(Self::Null),
            (Self::Integer(value), DataType::Integer) => Some(Self::Integer(*value)),
            (Self::Integer(value), DataType::Float) => Some(Self::Float(*value as f64)),
            (Self::Float(value), DataType::Float) => Some(Self::Float(*value)),
            (Self::Text(value), DataType::Text) => Some(Self::Text(value.clone())),
            (Self::Boolean(value), DataType::Boolean) => Some(Self::Boolean(*value)),
            _ => None,
        }
    }

    pub fn parse(value: &str, data_type: &DataType) -> Option<Self> {
        match data_type {
            DataType::Integer => value.parse().ok().map(Self::Integer),
            DataType::Float => value.parse().ok().map(Self::Float),
            DataType::Text => Some(Self::Text(value.to_string())),
            DataType::Boolean => match value.to_ascii_lowercase().as_str() {
                "true" | "1" | "yes" => Some(Self::Boolean(true)),
                "false" | "0" | "no" => Some(Self::Boolean(false)),
                _ => None,
            },
            DataType::Null => Some(Self::Null),
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    pub fn truth(&self) -> SqlTruth {
        match self {
            Self::Boolean(true) => SqlTruth::True,
            Self::Boolean(false) => SqlTruth::False,
            Self::Null => SqlTruth::Unknown,
            Self::Integer(value) => {
                if *value == 0 {
                    SqlTruth::False
                } else {
                    SqlTruth::True
                }
            }
            Self::Float(value) => {
                if *value == 0.0 {
                    SqlTruth::False
                } else {
                    SqlTruth::True
                }
            }
            Self::Text(value) => {
                if value.is_empty() {
                    SqlTruth::False
                } else {
                    SqlTruth::True
                }
            }
        }
    }

    pub fn compare_sql(&self, other: &Self, op: &ComparisonOp) -> SqlTruth {
        if self.is_null() || other.is_null() {
            return SqlTruth::Unknown;
        }
        let result = match op {
            ComparisonOp::Equal => self.partial_cmp(other) == Some(Ordering::Equal),
            ComparisonOp::NotEqual => self.partial_cmp(other) != Some(Ordering::Equal),
            ComparisonOp::LessThan => self.partial_cmp(other) == Some(Ordering::Less),
            ComparisonOp::LessThanOrEqual => {
                matches!(
                    self.partial_cmp(other),
                    Some(Ordering::Less | Ordering::Equal)
                )
            }
            ComparisonOp::GreaterThan => self.partial_cmp(other) == Some(Ordering::Greater),
            ComparisonOp::GreaterThanOrEqual => {
                matches!(
                    self.partial_cmp(other),
                    Some(Ordering::Greater | Ordering::Equal)
                )
            }
            ComparisonOp::Like => match (self, other) {
                (Self::Text(text), Self::Text(pattern)) => like_match(text, pattern),
                _ => false,
            },
        };
        SqlTruth::from(result)
    }

    /// Compatibility helper retained for existing callers.
    pub fn compare(&self, other: &Self, op: &ComparisonOp) -> bool {
        self.compare_sql(other, op) == SqlTruth::True
    }
}

impl PartialOrd for Value {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        match (self, other) {
            (Self::Integer(a), Self::Integer(b)) => a.partial_cmp(b),
            (Self::Float(a), Self::Float(b)) => a.partial_cmp(b),
            (Self::Integer(a), Self::Float(b)) => (*a as f64).partial_cmp(b),
            (Self::Float(a), Self::Integer(b)) => a.partial_cmp(&(*b as f64)),
            (Self::Text(a), Self::Text(b)) => a.partial_cmp(b),
            (Self::Boolean(a), Self::Boolean(b)) => a.partial_cmp(b),
            (Self::Null, Self::Null) => Some(Ordering::Equal),
            _ => None,
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Integer(value) => write!(f, "{value}"),
            Self::Float(value) => write!(f, "{value}"),
            Self::Text(value) => write!(f, "{value}"),
            Self::Boolean(value) => write!(f, "{value}"),
            Self::Null => write!(f, "NULL"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqlTruth {
    True,
    False,
    Unknown,
}

impl SqlTruth {
    pub fn and(self, other: Self) -> Self {
        match (self, other) {
            (Self::False, _) | (_, Self::False) => Self::False,
            (Self::True, Self::True) => Self::True,
            _ => Self::Unknown,
        }
    }

    pub fn or(self, other: Self) -> Self {
        match (self, other) {
            (Self::True, _) | (_, Self::True) => Self::True,
            (Self::False, Self::False) => Self::False,
            _ => Self::Unknown,
        }
    }

    pub fn negate(self) -> Self {
        match self {
            Self::True => Self::False,
            Self::False => Self::True,
            Self::Unknown => Self::Unknown,
        }
    }
}

impl From<bool> for SqlTruth {
    fn from(value: bool) -> Self {
        if value { Self::True } else { Self::False }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ComparisonOp {
    Equal,
    NotEqual,
    LessThan,
    LessThanOrEqual,
    GreaterThan,
    GreaterThanOrEqual,
    Like,
}

impl fmt::Display for ComparisonOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Self::Equal => "=",
                Self::NotEqual => "!=",
                Self::LessThan => "<",
                Self::LessThanOrEqual => "<=",
                Self::GreaterThan => ">",
                Self::GreaterThanOrEqual => ">=",
                Self::Like => "LIKE",
            }
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LogicalOp {
    And,
    Or,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BinaryOp {
    Add,
    Subtract,
    Multiply,
    Divide,
    Compare(ComparisonOp),
    And,
    Or,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnaryOp {
    Not,
    Negate,
    Plus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AggregateFunction {
    Count,
    Sum,
    Avg,
    Max,
    Min,
}

/// RustyDB-owned expression tree. Subqueries are boxed to keep recursive values bounded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Expr {
    Column {
        qualifier: Option<String>,
        name: String,
    },
    Literal(Value),
    /// Positional `?` parameter assigned during statement preparation.
    Parameter(usize),
    /// Value proposed by an INSERT ... ON DUPLICATE KEY UPDATE statement.
    Incoming(String),
    Binary {
        left: Box<Expr>,
        op: BinaryOp,
        right: Box<Expr>,
    },
    Unary {
        op: UnaryOp,
        expr: Box<Expr>,
    },
    IsNull {
        expr: Box<Expr>,
        negated: bool,
    },
    Like {
        expr: Box<Expr>,
        pattern: Box<Expr>,
        negated: bool,
    },
    InList {
        expr: Box<Expr>,
        list: Vec<Expr>,
        negated: bool,
    },
    InSubquery {
        expr: Box<Expr>,
        query: Box<Query>,
        negated: bool,
    },
    Exists {
        query: Box<Query>,
        negated: bool,
    },
    ScalarSubquery(Box<Query>),
    Aggregate {
        function: AggregateFunction,
        expr: Option<Box<Expr>>,
        distinct: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SelectItem {
    Wildcard(Option<String>),
    Expr { expr: Expr, alias: Option<String> },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TableSource {
    Table { name: String, alias: Option<String> },
    Derived { query: Box<Query>, alias: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum JoinType {
    Inner,
    Left,
    Right,
    Cross,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Join {
    pub join_type: JoinType,
    pub source: TableSource,
    pub on: Option<Expr>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrderBy {
    pub expr: Expr,
    pub descending: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cte {
    pub name: String,
    pub columns: Vec<String>,
    pub query: Box<Query>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Query {
    pub ctes: Vec<Cte>,
    pub distinct: bool,
    pub projection: Vec<SelectItem>,
    pub from: Option<TableSource>,
    pub joins: Vec<Join>,
    pub selection: Option<Expr>,
    pub group_by: Vec<Expr>,
    pub having: Option<Expr>,
    pub order_by: Vec<OrderBy>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum JoinAlgorithm {
    Hash,
    NestedLoop,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum LogicalPlan {
    Values,
    TableScan {
        table: String,
        alias: Option<String>,
        index: Option<String>,
    },
    DerivedScan {
        alias: String,
        input: Box<LogicalPlan>,
    },
    Filter {
        predicate: Expr,
        input: Box<LogicalPlan>,
    },
    Projection {
        expressions: Vec<SelectItem>,
        input: Box<LogicalPlan>,
    },
    Join {
        join_type: JoinType,
        algorithm: JoinAlgorithm,
        on: Option<Expr>,
        left: Box<LogicalPlan>,
        right: Box<LogicalPlan>,
    },
    Aggregate {
        group_by: Vec<Expr>,
        having: Option<Expr>,
        input: Box<LogicalPlan>,
    },
    Sort {
        order_by: Vec<OrderBy>,
        input: Box<LogicalPlan>,
    },
    TopN {
        order_by: Vec<OrderBy>,
        limit: usize,
        input: Box<LogicalPlan>,
    },
    Limit {
        limit: usize,
        input: Box<LogicalPlan>,
    },
    MaterializeCte {
        name: String,
        cte: Box<LogicalPlan>,
        input: Box<LogicalPlan>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ForeignKeyConstraint {
    pub name: Option<String>,
    pub columns: Vec<String>,
    pub foreign_table: String,
    pub referred_columns: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TableConstraint {
    PrimaryKey {
        name: Option<String>,
        columns: Vec<String>,
    },
    Unique {
        name: Option<String>,
        columns: Vec<String>,
    },
    Check {
        name: Option<String>,
        expr: Expr,
    },
    ForeignKey(ForeignKeyConstraint),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexDef {
    pub name: String,
    pub columns: Vec<String>,
    pub unique: bool,
    pub automatic: bool,
}

/// Column definition for CREATE TABLE.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColumnDef {
    pub name: String,
    pub data_type: DataType,
    pub nullable: bool,
    pub primary_key: bool,
    #[serde(default)]
    pub unique: bool,
    pub default: Option<Value>,
    #[serde(default)]
    pub checks: Vec<Expr>,
}

impl ColumnDef {
    pub fn new(name: &str, data_type: DataType) -> Self {
        Self {
            name: name.to_string(),
            data_type,
            nullable: true,
            primary_key: false,
            unique: false,
            default: None,
            checks: Vec::new(),
        }
    }

    pub fn not_null(mut self) -> Self {
        self.nullable = false;
        self
    }

    pub fn primary_key(mut self) -> Self {
        self.primary_key = true;
        self.unique = true;
        self.nullable = false;
        self
    }

    pub fn unique(mut self) -> Self {
        self.unique = true;
        self
    }

    pub fn with_default(mut self, value: Value) -> Self {
        self.default = Some(value);
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableSchema {
    pub name: String,
    pub columns: Vec<ColumnDef>,
    #[serde(default)]
    pub constraints: Vec<TableConstraint>,
    #[serde(default)]
    pub indexes: Vec<IndexDef>,
}

impl TableSchema {
    pub fn new(name: &str, columns: Vec<ColumnDef>) -> Self {
        Self {
            name: name.to_string(),
            columns,
            constraints: Vec::new(),
            indexes: Vec::new(),
        }
    }

    pub fn column_index(&self, name: &str) -> Option<usize> {
        self.columns
            .iter()
            .position(|column| column.name.eq_ignore_ascii_case(name))
    }

    pub fn column(&self, name: &str) -> Option<&ColumnDef> {
        self.columns
            .iter()
            .find(|column| column.name.eq_ignore_ascii_case(name))
    }

    pub fn primary_key_index(&self) -> Option<usize> {
        self.columns.iter().position(|column| column.primary_key)
    }

    pub fn validate_row(&self, row: &[Value]) -> Result<(), String> {
        if row.len() != self.columns.len() {
            return Err(format!(
                "Expected {} columns, got {}",
                self.columns.len(),
                row.len()
            ));
        }
        for (value, column) in row.iter().zip(&self.columns) {
            if value.is_null() {
                if !column.nullable {
                    return Err(format!("Column '{}' cannot be NULL", column.name));
                }
            } else if !value.is_compatible_with(&column.data_type) {
                return Err(format!(
                    "Column '{}' expects {}, got {}",
                    column.name,
                    column.data_type,
                    value.data_type()
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Row {
    pub values: Vec<Value>,
}

impl Row {
    pub fn new(values: Vec<Value>) -> Self {
        Self { values }
    }

    pub fn get(&self, index: usize) -> Option<&Value> {
        self.values.get(index)
    }
}

#[derive(Debug, Clone)]
pub struct ResultSet {
    pub columns: Vec<String>,
    pub rows: Vec<Row>,
}

impl ResultSet {
    pub fn new(columns: Vec<String>) -> Self {
        Self {
            columns,
            rows: Vec::new(),
        }
    }

    pub fn add_row(&mut self, row: Row) {
        self.rows.push(row);
    }

    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    pub fn to_table_string(&self) -> String {
        if self.columns.is_empty() {
            return "Empty result set".to_string();
        }
        let mut widths: Vec<usize> = self.columns.iter().map(String::len).collect();
        for row in &self.rows {
            for (index, value) in row.values.iter().enumerate() {
                if let Some(width) = widths.get_mut(index) {
                    *width = (*width).max(value.to_string().len());
                }
            }
        }
        let separator = format!(
            "+{}+\n",
            widths
                .iter()
                .map(|width| "-".repeat(width + 2))
                .collect::<Vec<_>>()
                .join("+")
        );
        let mut output = separator.clone();
        output.push('|');
        for (index, column) in self.columns.iter().enumerate() {
            output.push_str(&format!(" {:width$} |", column, width = widths[index]));
        }
        output.push('\n');
        output.push_str(&separator);
        for row in &self.rows {
            output.push('|');
            for (index, value) in row.values.iter().enumerate() {
                output.push_str(&format!(" {:width$} |", value, width = widths[index]));
            }
            output.push('\n');
        }
        output.push_str(&separator);
        output.push_str(&format!("{} row(s)\n", self.rows.len()));
        output
    }
}

/// SQL LIKE pattern matching with `%` and `_`.
pub(crate) fn like_match(text: &str, pattern: &str) -> bool {
    let text = text.to_lowercase();
    let pattern = pattern.to_lowercase();
    let text: Vec<char> = text.chars().collect();
    let pattern: Vec<char> = pattern.chars().collect();
    let mut dp = vec![vec![false; pattern.len() + 1]; text.len() + 1];
    dp[0][0] = true;
    for j in 1..=pattern.len() {
        if pattern[j - 1] == '%' {
            dp[0][j] = dp[0][j - 1];
        }
    }
    for i in 1..=text.len() {
        for j in 1..=pattern.len() {
            dp[i][j] = match pattern[j - 1] {
                '%' => dp[i][j - 1] || dp[i - 1][j],
                '_' => dp[i - 1][j - 1],
                character => character == text[i - 1] && dp[i - 1][j - 1],
            };
        }
    }
    dp[text.len()][pattern.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_value_comparison() {
        assert!(Value::Integer(5).compare(&Value::Integer(3), &ComparisonOp::GreaterThan));
        assert_eq!(
            Value::Null.compare_sql(&Value::Integer(1), &ComparisonOp::Equal),
            SqlTruth::Unknown
        );
    }

    #[test]
    fn test_like_pattern() {
        assert!(like_match("hello world", "hello%"));
        assert!(like_match("Hello World", "hello%"));
        assert!(like_match("cat", "c_t"));
        assert!(!like_match("cat", "d%"));
    }

    #[test]
    fn test_schema_validation() {
        let schema = TableSchema::new(
            "users",
            vec![
                ColumnDef::new("id", DataType::Integer).primary_key(),
                ColumnDef::new("name", DataType::Text).not_null(),
            ],
        );
        assert!(
            schema
                .validate_row(&[Value::Integer(1), Value::Text("Alice".into())])
                .is_ok()
        );
        assert!(
            schema
                .validate_row(&[Value::Integer(1), Value::Null])
                .is_err()
        );
    }
}
