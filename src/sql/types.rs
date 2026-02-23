use serde::{Deserialize, Serialize};
use std::fmt;

/// Supported SQL data types
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DataType {
    Integer,
    Float,
    Text,
    Boolean,
    Null,
}

impl fmt::Display for DataType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DataType::Integer => write!(f, "INTEGER"),
            DataType::Float => write!(f, "FLOAT"),
            DataType::Text => write!(f, "TEXT"),
            DataType::Boolean => write!(f, "BOOLEAN"),
            DataType::Null => write!(f, "NULL"),
        }
    }
}

/// A value in the database
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Value {
    Integer(i64),
    Float(f64),
    Text(String),
    Boolean(bool),
    Null,
}

impl Value {
    /// Get the data type of this value
    pub fn data_type(&self) -> DataType {
        match self {
            Value::Integer(_) => DataType::Integer,
            Value::Float(_) => DataType::Float,
            Value::Text(_) => DataType::Text,
            Value::Boolean(_) => DataType::Boolean,
            Value::Null => DataType::Null,
        }
    }

    /// Check if this value is compatible with a data type
    pub fn is_compatible_with(&self, dtype: &DataType) -> bool {
        match (self, dtype) {
            (Value::Null, _) => true, // NULL is compatible with any type
            (Value::Integer(_), DataType::Integer) => true,
            (Value::Integer(_), DataType::Float) => true, // Int can be promoted to Float
            (Value::Float(_), DataType::Float) => true,
            (Value::Text(_), DataType::Text) => true,
            (Value::Boolean(_), DataType::Boolean) => true,
            _ => false,
        }
    }

    /// Convert value to the target type if possible
    #[allow(dead_code)]
    pub fn coerce_to(&self, dtype: &DataType) -> Option<Value> {
        match (self, dtype) {
            (Value::Null, _) => Some(Value::Null),
            (Value::Integer(i), DataType::Integer) => Some(Value::Integer(*i)),
            (Value::Integer(i), DataType::Float) => Some(Value::Float(*i as f64)),
            (Value::Float(f), DataType::Float) => Some(Value::Float(*f)),
            (Value::Text(s), DataType::Text) => Some(Value::Text(s.clone())),
            (Value::Boolean(b), DataType::Boolean) => Some(Value::Boolean(*b)),
            _ => None,
        }
    }

    /// Parse a string into a value of the given type
    #[allow(dead_code)]
    pub fn parse(s: &str, dtype: &DataType) -> Option<Value> {
        match dtype {
            DataType::Integer => s.parse::<i64>().ok().map(Value::Integer),
            DataType::Float => s.parse::<f64>().ok().map(Value::Float),
            DataType::Text => Some(Value::Text(s.to_string())),
            DataType::Boolean => match s.to_lowercase().as_str() {
                "true" | "1" | "yes" => Some(Value::Boolean(true)),
                "false" | "0" | "no" => Some(Value::Boolean(false)),
                _ => None,
            },
            DataType::Null => Some(Value::Null),
        }
    }

    /// Compare two values (for WHERE clauses)
    pub fn compare(&self, other: &Value, op: &ComparisonOp) -> bool {
        match op {
            ComparisonOp::Equal => self == other,
            ComparisonOp::NotEqual => self != other,
            ComparisonOp::LessThan => self.partial_cmp(other) == Some(std::cmp::Ordering::Less),
            ComparisonOp::LessThanOrEqual => {
                matches!(self.partial_cmp(other), Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal))
            }
            ComparisonOp::GreaterThan => self.partial_cmp(other) == Some(std::cmp::Ordering::Greater),
            ComparisonOp::GreaterThanOrEqual => {
                matches!(self.partial_cmp(other), Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal))
            }
            ComparisonOp::Like => {
                if let (Value::Text(s), Value::Text(pattern)) = (self, other) {
                    like_match(s, pattern)
                } else {
                    false
                }
            }
        }
    }
}

impl PartialOrd for Value {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        match (self, other) {
            (Value::Integer(a), Value::Integer(b)) => a.partial_cmp(b),
            (Value::Float(a), Value::Float(b)) => a.partial_cmp(b),
            (Value::Integer(a), Value::Float(b)) => (*a as f64).partial_cmp(b),
            (Value::Float(a), Value::Integer(b)) => a.partial_cmp(&(*b as f64)),
            (Value::Text(a), Value::Text(b)) => a.partial_cmp(b),
            (Value::Boolean(a), Value::Boolean(b)) => a.partial_cmp(b),
            (Value::Null, Value::Null) => Some(std::cmp::Ordering::Equal),
            _ => None,
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Integer(i) => write!(f, "{}", i),
            Value::Float(fl) => write!(f, "{}", fl),
            Value::Text(s) => write!(f, "{}", s),
            Value::Boolean(b) => write!(f, "{}", b),
            Value::Null => write!(f, "NULL"),
        }
    }
}

/// Comparison operators for WHERE clauses
#[derive(Debug, Clone, PartialEq)]
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
        match self {
            ComparisonOp::Equal => write!(f, "="),
            ComparisonOp::NotEqual => write!(f, "!="),
            ComparisonOp::LessThan => write!(f, "<"),
            ComparisonOp::LessThanOrEqual => write!(f, "<="),
            ComparisonOp::GreaterThan => write!(f, ">"),
            ComparisonOp::GreaterThanOrEqual => write!(f, ">="),
            ComparisonOp::Like => write!(f, "LIKE"),
        }
    }
}

/// Logical operators for combining conditions
#[derive(Debug, Clone, PartialEq)]
pub enum LogicalOp {
    And,
    Or,
}

/// Column definition for CREATE TABLE
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColumnDef {
    pub name: String,
    pub data_type: DataType,
    pub nullable: bool,
    pub primary_key: bool,
    pub default: Option<Value>,
}

impl ColumnDef {
    pub fn new(name: &str, data_type: DataType) -> Self {
        ColumnDef {
            name: name.to_string(),
            data_type,
            nullable: true,
            primary_key: false,
            default: None,
        }
    }

    pub fn not_null(mut self) -> Self {
        self.nullable = false;
        self
    }

    pub fn primary_key(mut self) -> Self {
        self.primary_key = true;
        self.nullable = false;
        self
    }

    #[allow(dead_code)]
    pub fn with_default(mut self, value: Value) -> Self {
        self.default = Some(value);
        self
    }
}

/// Table schema
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableSchema {
    pub name: String,
    pub columns: Vec<ColumnDef>,
}

impl TableSchema {
    pub fn new(name: &str, columns: Vec<ColumnDef>) -> Self {
        TableSchema {
            name: name.to_string(),
            columns,
        }
    }

    /// Get column index by name
    pub fn column_index(&self, name: &str) -> Option<usize> {
        let name_lower = name.to_lowercase();
        self.columns.iter().position(|c| c.name.to_lowercase() == name_lower)
    }

    /// Get column definition by name
    pub fn column(&self, name: &str) -> Option<&ColumnDef> {
        let name_lower = name.to_lowercase();
        self.columns.iter().find(|c| c.name.to_lowercase() == name_lower)
    }

    /// Get primary key column index
    pub fn primary_key_index(&self) -> Option<usize> {
        self.columns.iter().position(|c| c.primary_key)
    }

    /// Validate a row against the schema
    pub fn validate_row(&self, row: &[Value]) -> Result<(), String> {
        if row.len() != self.columns.len() {
            return Err(format!(
                "Expected {} columns, got {}",
                self.columns.len(),
                row.len()
            ));
        }

        for (_i, (value, col)) in row.iter().zip(self.columns.iter()).enumerate() {
            if matches!(value, Value::Null) {
                if !col.nullable {
                    return Err(format!("Column '{}' cannot be NULL", col.name));
                }
            } else if !value.is_compatible_with(&col.data_type) {
                return Err(format!(
                    "Column '{}' expects {:?}, got {:?}",
                    col.name,
                    col.data_type,
                    value.data_type()
                ));
            }
        }

        Ok(())
    }
}

/// A row in a table
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Row {
    pub values: Vec<Value>,
}

impl Row {
    pub fn new(values: Vec<Value>) -> Self {
        Row { values }
    }

    #[allow(dead_code)]
    pub fn get(&self, index: usize) -> Option<&Value> {
        self.values.get(index)
    }
}

/// Result set from a SELECT query
#[derive(Debug, Clone)]
pub struct ResultSet {
    pub columns: Vec<String>,
    pub rows: Vec<Row>,
}

impl ResultSet {
    pub fn new(columns: Vec<String>) -> Self {
        ResultSet {
            columns,
            rows: Vec::new(),
        }
    }

    pub fn add_row(&mut self, row: Row) {
        self.rows.push(row);
    }

    #[allow(dead_code)]
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    /// Format as a table string
    pub fn to_table_string(&self) -> String {
        if self.columns.is_empty() {
            return String::from("Empty result set");
        }

        // Calculate column widths
        let mut widths: Vec<usize> = self.columns.iter().map(|c| c.len()).collect();
        for row in &self.rows {
            for (i, value) in row.values.iter().enumerate() {
                if i < widths.len() {
                    widths[i] = widths[i].max(value.to_string().len());
                }
            }
        }

        let mut result = String::new();

        // Header separator
        let separator: String = widths.iter().map(|w| "-".repeat(*w + 2)).collect::<Vec<_>>().join("+");
        let separator = format!("+{}+\n", separator);

        result.push_str(&separator);

        // Header
        let header: String = self.columns
            .iter()
            .enumerate()
            .map(|(i, c)| format!(" {:width$} ", c, width = widths[i]))
            .collect::<Vec<_>>()
            .join("|");
        result.push_str(&format!("|{}|\n", header));
        result.push_str(&separator);

        // Rows
        for row in &self.rows {
            let row_str: String = row.values
                .iter()
                .enumerate()
                .map(|(i, v)| {
                    let width = widths.get(i).copied().unwrap_or(10);
                    format!(" {:width$} ", v.to_string(), width = width)
                })
                .collect::<Vec<_>>()
                .join("|");
            result.push_str(&format!("|{}|\n", row_str));
        }

        result.push_str(&separator);
        result.push_str(&format!("{} row(s)\n", self.rows.len()));

        result
    }
}

/// SQL LIKE pattern matching (supports % and _)
fn like_match(text: &str, pattern: &str) -> bool {
    let text_chars: Vec<char> = text.chars().collect();
    let pattern_chars: Vec<char> = pattern.chars().collect();
    
    fn match_helper(text: &[char], pattern: &[char]) -> bool {
        match (text.first(), pattern.first()) {
            (None, None) => true,
            (None, Some('%')) => match_helper(text, &pattern[1..]),
            (None, Some(_)) => false,
            (Some(_), None) => false,
            (Some(_), Some('%')) => {
                // % matches zero or more characters
                match_helper(text, &pattern[1..]) || match_helper(&text[1..], pattern)
            }
            (Some(_t), Some('_')) => {
                // _ matches exactly one character
                match_helper(&text[1..], &pattern[1..])
            }
            (Some(t), Some(p)) => {
                if t.to_lowercase().next() == p.to_lowercase().next() {
                    match_helper(&text[1..], &pattern[1..])
                } else {
                    false
                }
            }
        }
    }
    
    match_helper(&text_chars, &pattern_chars)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_value_comparison() {
        assert!(Value::Integer(5).compare(&Value::Integer(5), &ComparisonOp::Equal));
        assert!(Value::Integer(5).compare(&Value::Integer(3), &ComparisonOp::GreaterThan));
        assert!(Value::Text("hello".to_string()).compare(&Value::Text("hello".to_string()), &ComparisonOp::Equal));
    }

    #[test]
    fn test_like_pattern() {
        assert!(like_match("hello", "hello"));
        assert!(like_match("hello", "h%"));
        assert!(like_match("hello", "%llo"));
        assert!(like_match("hello", "%ll%"));
        assert!(like_match("hello", "h_llo"));
        assert!(!like_match("hello", "h_lo"));
    }

    #[test]
    fn test_schema_validation() {
        let schema = TableSchema::new("test", vec![
            ColumnDef::new("id", DataType::Integer).primary_key(),
            ColumnDef::new("name", DataType::Text).not_null(),
            ColumnDef::new("age", DataType::Integer),
        ]);

        let valid_row = vec![Value::Integer(1), Value::Text("Alice".to_string()), Value::Integer(30)];
        assert!(schema.validate_row(&valid_row).is_ok());

        let null_name = vec![Value::Integer(1), Value::Null, Value::Integer(30)];
        assert!(schema.validate_row(&null_name).is_err());
    }
}
