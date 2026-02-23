use super::types::*;

/// Parsed SQL statement
#[derive(Debug, Clone)]
pub enum Statement {
    CreateTable {
        table_name: String,
        columns: Vec<ColumnDef>,
        if_not_exists: bool,
    },
    DropTable {
        table_name: String,
        if_exists: bool,
    },
    Insert {
        table_name: String,
        columns: Option<Vec<String>>,
        values: Vec<Vec<Value>>,
    },
    Select {
        columns: SelectColumns,
        table_name: String,
        where_clause: Option<WhereClause>,
        order_by: Option<OrderBy>,
        limit: Option<usize>,
    },
    Update {
        table_name: String,
        assignments: Vec<(String, Value)>,
        where_clause: Option<WhereClause>,
    },
    Delete {
        table_name: String,
        where_clause: Option<WhereClause>,
    },
    ShowTables,
    DescribeTable {
        table_name: String,
    },
}

/// SELECT column specification
#[derive(Debug, Clone)]
pub enum SelectColumns {
    All,
    Columns(Vec<String>),
}

/// WHERE clause representation
#[derive(Debug, Clone)]
pub struct WhereClause {
    pub conditions: Vec<Condition>,
    pub logical_ops: Vec<LogicalOp>,
}

impl WhereClause {
    pub fn single(condition: Condition) -> Self {
        WhereClause {
            conditions: vec![condition],
            logical_ops: vec![],
        }
    }

    pub fn and(mut self, condition: Condition) -> Self {
        self.conditions.push(condition);
        self.logical_ops.push(LogicalOp::And);
        self
    }

    pub fn or(mut self, condition: Condition) -> Self {
        self.conditions.push(condition);
        self.logical_ops.push(LogicalOp::Or);
        self
    }
}

/// A single condition in a WHERE clause
#[derive(Debug, Clone)]
pub struct Condition {
    pub column: String,
    pub op: ComparisonOp,
    pub value: Value,
}

/// ORDER BY specification
#[derive(Debug, Clone)]
pub struct OrderBy {
    pub column: String,
    pub descending: bool,
}

/// SQL Parser
pub struct Parser {
    tokens: Vec<Token>,
    position: usize,
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    // Keywords
    Create,
    Table,
    Drop,
    Insert,
    Into,
    Values,
    Select,
    From,
    Where,
    Update,
    Set,
    Delete,
    And,
    Or,
    Not,
    Null,
    Primary,
    Key,
    If,
    Exists,
    Show,
    Tables,
    Describe,
    Desc,
    Order,
    By,
    Asc,
    Limit,
    Like,
    // Types
    IntegerType,
    FloatType,
    TextType,
    VarcharType,
    BooleanType,
    // Symbols
    LeftParen,
    RightParen,
    Comma,
    Semicolon,
    Star,
    Equals,
    NotEquals,
    LessThan,
    LessThanOrEqual,
    GreaterThan,
    GreaterThanOrEqual,
    // Literals
    Identifier(String),
    StringLiteral(String),
    IntegerLiteral(i64),
    FloatLiteral(f64),
    True,
    False,
}

impl Parser {
    pub fn new(sql: &str) -> Self {
        let tokens = Self::tokenize(sql);
        Parser { tokens, position: 0 }
    }

    /// Parse a SQL statement
    pub fn parse(&mut self) -> Result<Statement, String> {
        self.position = 0;
        
        let stmt = match self.peek() {
            Some(Token::Create) => self.parse_create(),
            Some(Token::Drop) => self.parse_drop(),
            Some(Token::Insert) => self.parse_insert(),
            Some(Token::Select) => self.parse_select(),
            Some(Token::Update) => self.parse_update(),
            Some(Token::Delete) => self.parse_delete(),
            Some(Token::Show) => self.parse_show(),
            Some(Token::Describe) | Some(Token::Desc) => self.parse_describe(),
            Some(token) => Err(format!("Unexpected token: {:?}", token)),
            None => Err("Empty statement".to_string()),
        }?;

        // Optional semicolon at end
        if self.peek() == Some(&Token::Semicolon) {
            self.advance();
        }

        Ok(stmt)
    }

    fn tokenize(sql: &str) -> Vec<Token> {
        let mut tokens = Vec::new();
        let mut chars = sql.chars().peekable();

        while let Some(&c) = chars.peek() {
            match c {
                // Whitespace
                ' ' | '\t' | '\n' | '\r' => {
                    chars.next();
                }
                // Symbols
                '(' => {
                    tokens.push(Token::LeftParen);
                    chars.next();
                }
                ')' => {
                    tokens.push(Token::RightParen);
                    chars.next();
                }
                ',' => {
                    tokens.push(Token::Comma);
                    chars.next();
                }
                ';' => {
                    tokens.push(Token::Semicolon);
                    chars.next();
                }
                '*' => {
                    tokens.push(Token::Star);
                    chars.next();
                }
                '=' => {
                    tokens.push(Token::Equals);
                    chars.next();
                }
                '<' => {
                    chars.next();
                    if chars.peek() == Some(&'=') {
                        chars.next();
                        tokens.push(Token::LessThanOrEqual);
                    } else if chars.peek() == Some(&'>') {
                        chars.next();
                        tokens.push(Token::NotEquals);
                    } else {
                        tokens.push(Token::LessThan);
                    }
                }
                '>' => {
                    chars.next();
                    if chars.peek() == Some(&'=') {
                        chars.next();
                        tokens.push(Token::GreaterThanOrEqual);
                    } else {
                        tokens.push(Token::GreaterThan);
                    }
                }
                '!' => {
                    chars.next();
                    if chars.peek() == Some(&'=') {
                        chars.next();
                        tokens.push(Token::NotEquals);
                    }
                }
                // String literal
                '\'' | '"' => {
                    let quote = chars.next().unwrap();
                    let mut string = String::new();
                    while let Some(&c) = chars.peek() {
                        if c == quote {
                            chars.next();
                            break;
                        }
                        // Handle escape sequences
                        if c == '\\' {
                            chars.next();
                            if let Some(&escaped) = chars.peek() {
                                string.push(match escaped {
                                    'n' => '\n',
                                    't' => '\t',
                                    'r' => '\r',
                                    _ => escaped,
                                });
                                chars.next();
                            }
                        } else {
                            string.push(chars.next().unwrap());
                        }
                    }
                    tokens.push(Token::StringLiteral(string));
                }
                // Number
                '0'..='9' | '-' => {
                    let mut num_str = String::new();
                    if c == '-' {
                        num_str.push(chars.next().unwrap());
                    }
                    while let Some(&c) = chars.peek() {
                        if c.is_ascii_digit() || c == '.' {
                            num_str.push(chars.next().unwrap());
                        } else {
                            break;
                        }
                    }
                    if num_str == "-" {
                        // Just a minus sign, not a number
                        continue;
                    }
                    if num_str.contains('.') {
                        if let Ok(f) = num_str.parse::<f64>() {
                            tokens.push(Token::FloatLiteral(f));
                        }
                    } else if let Ok(i) = num_str.parse::<i64>() {
                        tokens.push(Token::IntegerLiteral(i));
                    }
                }
                // Identifier or keyword
                'a'..='z' | 'A'..='Z' | '_' => {
                    let mut ident = String::new();
                    while let Some(&c) = chars.peek() {
                        if c.is_alphanumeric() || c == '_' {
                            ident.push(chars.next().unwrap());
                        } else {
                            break;
                        }
                    }
                    let token = match ident.to_uppercase().as_str() {
                        "CREATE" => Token::Create,
                        "TABLE" => Token::Table,
                        "DROP" => Token::Drop,
                        "INSERT" => Token::Insert,
                        "INTO" => Token::Into,
                        "VALUES" => Token::Values,
                        "SELECT" => Token::Select,
                        "FROM" => Token::From,
                        "WHERE" => Token::Where,
                        "UPDATE" => Token::Update,
                        "SET" => Token::Set,
                        "DELETE" => Token::Delete,
                        "AND" => Token::And,
                        "OR" => Token::Or,
                        "NOT" => Token::Not,
                        "NULL" => Token::Null,
                        "PRIMARY" => Token::Primary,
                        "KEY" => Token::Key,
                        "IF" => Token::If,
                        "EXISTS" => Token::Exists,
                        "SHOW" => Token::Show,
                        "TABLES" => Token::Tables,
                        "DESCRIBE" => Token::Describe,
                        "DESC" => Token::Desc,
                        "ORDER" => Token::Order,
                        "BY" => Token::By,
                        "ASC" => Token::Asc,
                        "LIMIT" => Token::Limit,
                        "LIKE" => Token::Like,
                        "INTEGER" | "INT" | "BIGINT" => Token::IntegerType,
                        "FLOAT" | "DOUBLE" | "REAL" | "DECIMAL" => Token::FloatType,
                        "TEXT" | "STRING" | "CHAR" => Token::TextType,
                        "VARCHAR" => Token::VarcharType,
                        "BOOLEAN" | "BOOL" => Token::BooleanType,
                        "TRUE" => Token::True,
                        "FALSE" => Token::False,
                        _ => Token::Identifier(ident),
                    };
                    tokens.push(token);
                }
                _ => {
                    chars.next(); // Skip unknown characters
                }
            }
        }

        tokens
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.position)
    }

    fn advance(&mut self) -> Option<&Token> {
        let token = self.tokens.get(self.position);
        self.position += 1;
        token
    }

    fn expect(&mut self, expected: Token) -> Result<(), String> {
        match self.advance() {
            Some(token) if *token == expected => Ok(()),
            Some(token) => Err(format!("Expected {:?}, got {:?}", expected, token)),
            None => Err(format!("Expected {:?}, got end of input", expected)),
        }
    }

    fn parse_identifier(&mut self) -> Result<String, String> {
        match self.advance() {
            Some(Token::Identifier(name)) => Ok(name.clone()),
            Some(token) => Err(format!("Expected identifier, got {:?}", token)),
            None => Err("Expected identifier, got end of input".to_string()),
        }
    }

    fn parse_create(&mut self) -> Result<Statement, String> {
        self.expect(Token::Create)?;
        self.expect(Token::Table)?;

        let if_not_exists = if self.peek() == Some(&Token::If) {
            self.advance();
            self.expect(Token::Not)?;
            self.expect(Token::Exists)?;
            true
        } else {
            false
        };

        let table_name = self.parse_identifier()?;
        self.expect(Token::LeftParen)?;

        let mut columns = Vec::new();
        loop {
            let col_name = self.parse_identifier()?;
            let data_type = self.parse_data_type()?;
            
            let mut col_def = ColumnDef::new(&col_name, data_type);

            // Parse column constraints
            loop {
                match self.peek() {
                    Some(Token::Primary) => {
                        self.advance();
                        self.expect(Token::Key)?;
                        col_def = col_def.primary_key();
                    }
                    Some(Token::Not) => {
                        self.advance();
                        self.expect(Token::Null)?;
                        col_def = col_def.not_null();
                    }
                    _ => break,
                }
            }

            columns.push(col_def);

            match self.peek() {
                Some(Token::Comma) => {
                    self.advance();
                }
                Some(Token::RightParen) => break,
                _ => return Err("Expected ',' or ')' in column list".to_string()),
            }
        }

        self.expect(Token::RightParen)?;

        Ok(Statement::CreateTable {
            table_name,
            columns,
            if_not_exists,
        })
    }

    fn parse_data_type(&mut self) -> Result<DataType, String> {
        match self.advance() {
            Some(Token::IntegerType) => Ok(DataType::Integer),
            Some(Token::FloatType) => Ok(DataType::Float),
            Some(Token::TextType) => Ok(DataType::Text),
            Some(Token::VarcharType) => {
                // VARCHAR(n) - we ignore the size for now
                if self.peek() == Some(&Token::LeftParen) {
                    self.advance();
                    // Skip the size
                    self.advance();
                    self.expect(Token::RightParen)?;
                }
                Ok(DataType::Text)
            }
            Some(Token::BooleanType) => Ok(DataType::Boolean),
            Some(token) => Err(format!("Expected data type, got {:?}", token)),
            None => Err("Expected data type, got end of input".to_string()),
        }
    }

    fn parse_drop(&mut self) -> Result<Statement, String> {
        self.expect(Token::Drop)?;
        self.expect(Token::Table)?;

        let if_exists = if self.peek() == Some(&Token::If) {
            self.advance();
            self.expect(Token::Exists)?;
            true
        } else {
            false
        };

        let table_name = self.parse_identifier()?;

        Ok(Statement::DropTable {
            table_name,
            if_exists,
        })
    }

    fn parse_insert(&mut self) -> Result<Statement, String> {
        self.expect(Token::Insert)?;
        self.expect(Token::Into)?;

        let table_name = self.parse_identifier()?;

        // Optional column list
        let columns = if self.peek() == Some(&Token::LeftParen) {
            self.advance();
            let mut cols = Vec::new();
            loop {
                cols.push(self.parse_identifier()?);
                match self.peek() {
                    Some(Token::Comma) => {
                        self.advance();
                    }
                    Some(Token::RightParen) => {
                        self.advance();
                        break;
                    }
                    _ => return Err("Expected ',' or ')' in column list".to_string()),
                }
            }
            Some(cols)
        } else {
            None
        };

        self.expect(Token::Values)?;

        // Parse value lists
        let mut values = Vec::new();
        loop {
            self.expect(Token::LeftParen)?;
            let mut row_values = Vec::new();
            loop {
                row_values.push(self.parse_value()?);
                match self.peek() {
                    Some(Token::Comma) => {
                        self.advance();
                    }
                    Some(Token::RightParen) => {
                        self.advance();
                        break;
                    }
                    _ => return Err("Expected ',' or ')' in value list".to_string()),
                }
            }
            values.push(row_values);

            // Check for more value sets
            if self.peek() == Some(&Token::Comma) {
                self.advance();
            } else {
                break;
            }
        }

        Ok(Statement::Insert {
            table_name,
            columns,
            values,
        })
    }

    fn parse_value(&mut self) -> Result<Value, String> {
        match self.advance() {
            Some(Token::IntegerLiteral(i)) => Ok(Value::Integer(*i)),
            Some(Token::FloatLiteral(f)) => Ok(Value::Float(*f)),
            Some(Token::StringLiteral(s)) => Ok(Value::Text(s.clone())),
            Some(Token::True) => Ok(Value::Boolean(true)),
            Some(Token::False) => Ok(Value::Boolean(false)),
            Some(Token::Null) => Ok(Value::Null),
            Some(token) => Err(format!("Expected value, got {:?}", token)),
            None => Err("Expected value, got end of input".to_string()),
        }
    }

    fn parse_select(&mut self) -> Result<Statement, String> {
        self.expect(Token::Select)?;

        // Parse columns
        let columns = if self.peek() == Some(&Token::Star) {
            self.advance();
            SelectColumns::All
        } else {
            let mut cols = Vec::new();
            loop {
                cols.push(self.parse_identifier()?);
                if self.peek() == Some(&Token::Comma) {
                    self.advance();
                } else {
                    break;
                }
            }
            SelectColumns::Columns(cols)
        };

        self.expect(Token::From)?;
        let table_name = self.parse_identifier()?;

        // Optional WHERE clause
        let where_clause = if self.peek() == Some(&Token::Where) {
            self.advance();
            Some(self.parse_where_clause()?)
        } else {
            None
        };

        // Optional ORDER BY
        let order_by = if self.peek() == Some(&Token::Order) {
            self.advance();
            self.expect(Token::By)?;
            let column = self.parse_identifier()?;
            let descending = if self.peek() == Some(&Token::Desc) {
                self.advance();
                true
            } else {
                if self.peek() == Some(&Token::Asc) {
                    self.advance();
                }
                false
            };
            Some(OrderBy { column, descending })
        } else {
            None
        };

        // Optional LIMIT
        let limit = if self.peek() == Some(&Token::Limit) {
            self.advance();
            match self.advance() {
                Some(Token::IntegerLiteral(n)) => Some(*n as usize),
                _ => return Err("Expected integer after LIMIT".to_string()),
            }
        } else {
            None
        };

        Ok(Statement::Select {
            columns,
            table_name,
            where_clause,
            order_by,
            limit,
        })
    }

    fn parse_where_clause(&mut self) -> Result<WhereClause, String> {
        let first_condition = self.parse_condition()?;
        let mut where_clause = WhereClause::single(first_condition);

        loop {
            match self.peek() {
                Some(Token::And) => {
                    self.advance();
                    let condition = self.parse_condition()?;
                    where_clause = where_clause.and(condition);
                }
                Some(Token::Or) => {
                    self.advance();
                    let condition = self.parse_condition()?;
                    where_clause = where_clause.or(condition);
                }
                _ => break,
            }
        }

        Ok(where_clause)
    }

    fn parse_condition(&mut self) -> Result<Condition, String> {
        let column = self.parse_identifier()?;
        let op = self.parse_comparison_op()?;
        let value = self.parse_value()?;

        Ok(Condition { column, op, value })
    }

    fn parse_comparison_op(&mut self) -> Result<ComparisonOp, String> {
        match self.advance() {
            Some(Token::Equals) => Ok(ComparisonOp::Equal),
            Some(Token::NotEquals) => Ok(ComparisonOp::NotEqual),
            Some(Token::LessThan) => Ok(ComparisonOp::LessThan),
            Some(Token::LessThanOrEqual) => Ok(ComparisonOp::LessThanOrEqual),
            Some(Token::GreaterThan) => Ok(ComparisonOp::GreaterThan),
            Some(Token::GreaterThanOrEqual) => Ok(ComparisonOp::GreaterThanOrEqual),
            Some(Token::Like) => Ok(ComparisonOp::Like),
            Some(token) => Err(format!("Expected comparison operator, got {:?}", token)),
            None => Err("Expected comparison operator, got end of input".to_string()),
        }
    }

    fn parse_update(&mut self) -> Result<Statement, String> {
        self.expect(Token::Update)?;
        let table_name = self.parse_identifier()?;
        self.expect(Token::Set)?;

        let mut assignments = Vec::new();
        loop {
            let column = self.parse_identifier()?;
            self.expect(Token::Equals)?;
            let value = self.parse_value()?;
            assignments.push((column, value));

            if self.peek() == Some(&Token::Comma) {
                self.advance();
            } else {
                break;
            }
        }

        let where_clause = if self.peek() == Some(&Token::Where) {
            self.advance();
            Some(self.parse_where_clause()?)
        } else {
            None
        };

        Ok(Statement::Update {
            table_name,
            assignments,
            where_clause,
        })
    }

    fn parse_delete(&mut self) -> Result<Statement, String> {
        self.expect(Token::Delete)?;
        self.expect(Token::From)?;
        let table_name = self.parse_identifier()?;

        let where_clause = if self.peek() == Some(&Token::Where) {
            self.advance();
            Some(self.parse_where_clause()?)
        } else {
            None
        };

        Ok(Statement::Delete {
            table_name,
            where_clause,
        })
    }

    fn parse_show(&mut self) -> Result<Statement, String> {
        self.expect(Token::Show)?;
        self.expect(Token::Tables)?;
        Ok(Statement::ShowTables)
    }

    fn parse_describe(&mut self) -> Result<Statement, String> {
        self.advance(); // DESCRIBE or DESC
        let table_name = self.parse_identifier()?;
        Ok(Statement::DescribeTable { table_name })
    }
}

/// Convenience function to parse SQL
pub fn parse_sql(sql: &str) -> Result<Statement, String> {
    let mut parser = Parser::new(sql);
    parser.parse()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_create_table() {
        let sql = "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL, age INTEGER)";
        let stmt = parse_sql(sql).unwrap();
        
        if let Statement::CreateTable { table_name, columns, .. } = stmt {
            assert_eq!(table_name, "users");
            assert_eq!(columns.len(), 3);
            assert!(columns[0].primary_key);
            assert!(!columns[1].nullable);
        } else {
            panic!("Expected CreateTable statement");
        }
    }

    #[test]
    fn test_parse_insert() {
        let sql = "INSERT INTO users (id, name) VALUES (1, 'Alice'), (2, 'Bob')";
        let stmt = parse_sql(sql).unwrap();
        
        if let Statement::Insert { table_name, columns, values } = stmt {
            assert_eq!(table_name, "users");
            assert_eq!(columns.unwrap().len(), 2);
            assert_eq!(values.len(), 2);
        } else {
            panic!("Expected Insert statement");
        }
    }

    #[test]
    fn test_parse_select() {
        let sql = "SELECT name, age FROM users WHERE age > 18 ORDER BY name LIMIT 10";
        let stmt = parse_sql(sql).unwrap();
        
        if let Statement::Select { columns: _, table_name, where_clause, order_by, limit } = stmt {
            assert_eq!(table_name, "users");
            assert!(where_clause.is_some());
            assert!(order_by.is_some());
            assert_eq!(limit, Some(10));
        } else {
            panic!("Expected Select statement");
        }
    }

    #[test]
    fn test_parse_update() {
        let sql = "UPDATE users SET name = 'Charlie' WHERE id = 1";
        let stmt = parse_sql(sql).unwrap();
        
        if let Statement::Update { table_name, assignments, where_clause } = stmt {
            assert_eq!(table_name, "users");
            assert_eq!(assignments.len(), 1);
            assert!(where_clause.is_some());
        } else {
            panic!("Expected Update statement");
        }
    }

    #[test]
    fn test_parse_delete() {
        let sql = "DELETE FROM users WHERE id = 1";
        let stmt = parse_sql(sql).unwrap();
        
        if let Statement::Delete { table_name, where_clause } = stmt {
            assert_eq!(table_name, "users");
            assert!(where_clause.is_some());
        } else {
            panic!("Expected Delete statement");
        }
    }
}
