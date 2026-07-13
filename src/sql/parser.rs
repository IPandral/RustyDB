use serde::{Deserialize, Serialize};
use sqlparser::ast as sql;
use sqlparser::dialect::MySqlDialect;
use sqlparser::parser::Parser;

use super::types::*;

/// Parsed RustyDB statement. The public AST is independent of sqlparser so the
/// execution and persistence layers are not coupled to a third-party AST.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Statement {
    CreateTable {
        table_name: String,
        columns: Vec<ColumnDef>,
        constraints: Vec<TableConstraint>,
        if_not_exists: bool,
    },
    DropTable {
        table_name: String,
        if_exists: bool,
    },
    CreateIndex {
        name: String,
        table_name: String,
        columns: Vec<String>,
        unique: bool,
        if_not_exists: bool,
    },
    DropIndex {
        name: String,
        table_name: String,
        if_exists: bool,
    },
    AlterTable {
        table_name: String,
        operations: Vec<AlterOperation>,
    },
    Insert {
        table_name: String,
        columns: Option<Vec<String>>,
        source: InsertSource,
        on_duplicate: Vec<(String, Expr)>,
    },
    Query(Query),
    Update {
        table_name: String,
        assignments: Vec<(String, Expr)>,
        selection: Option<Expr>,
    },
    Delete {
        table_name: String,
        selection: Option<Expr>,
    },
    ShowTables,
    ShowIndexes {
        table_name: String,
    },
    DescribeTable {
        table_name: String,
    },
    Explain(Box<Statement>),
    SetVariable {
        name: String,
        value: Value,
    },
    Prepare {
        name: String,
        source: PrepareSource,
    },
    ExecutePrepared {
        name: String,
        using: Vec<String>,
    },
    DeallocatePrepared {
        name: String,
    },
    Begin,
    Commit,
    Rollback,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PrepareSource {
    Sql(String),
    Variable(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum InsertSource {
    Values(Vec<Vec<Expr>>),
    Query(Box<Query>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AlterOperation {
    AddColumn {
        column: ColumnDef,
        constraints: Vec<TableConstraint>,
        if_not_exists: bool,
    },
    DropColumn {
        name: String,
        if_exists: bool,
    },
    RenameColumn {
        old_name: String,
        new_name: String,
    },
    ModifyColumn {
        old_name: String,
        column: ColumnDef,
        constraints: Vec<TableConstraint>,
    },
    SetDefault {
        column: String,
        value: Option<Value>,
    },
    SetNullable {
        column: String,
        nullable: bool,
    },
    SetDataType {
        column: String,
        data_type: DataType,
    },
    AddConstraint(TableConstraint),
    DropConstraint {
        name: String,
        if_exists: bool,
    },
    DropPrimaryKey,
}

/// Compatibility aliases retained for callers that used the original names.
pub type WhereClause = Expr;
pub type Condition = Expr;
pub type SelectColumns = Vec<SelectItem>;

pub fn parse_sql(input: &str) -> Result<Statement, String> {
    let trimmed = input.trim().trim_end_matches(';').trim();
    if trimmed.is_empty() {
        return Err("Empty statement".to_string());
    }

    if let Some(statement) = parse_rustydb_extension(trimmed)? {
        return Ok(statement);
    }

    let mut statements =
        Parser::parse_sql(&MySqlDialect {}, trimmed).map_err(|error| error.to_string())?;
    if statements.len() != 1 {
        return Err("Expected exactly one SQL statement".to_string());
    }
    let mut statement = convert_statement(statements.remove(0))?;
    assign_parameters(&mut statement);
    Ok(statement)
}

fn parse_rustydb_extension(sql: &str) -> Result<Option<Statement>, String> {
    let words: Vec<&str> = sql.split_whitespace().collect();
    let mysql_drop_constraint = words.len() >= 6
        && words[0].eq_ignore_ascii_case("ALTER")
        && words[1].eq_ignore_ascii_case("TABLE")
        && words[3].eq_ignore_ascii_case("DROP")
        && ((words.len() == 7
            && words[4].eq_ignore_ascii_case("FOREIGN")
            && words[5].eq_ignore_ascii_case("KEY"))
            || (words.len() == 6
                && (words[4].eq_ignore_ascii_case("CHECK")
                    || words[4].eq_ignore_ascii_case("INDEX"))));
    if mysql_drop_constraint {
        return Ok(Some(Statement::AlterTable {
            table_name: clean_identifier(words[2]),
            operations: vec![AlterOperation::DropConstraint {
                name: clean_identifier(words.last().unwrap()),
                if_exists: false,
            }],
        }));
    }
    if sql.len() >= 4 && sql[..4].eq_ignore_ascii_case("SET ") {
        let rest = sql[4..].trim();
        if let Some((name, value)) = rest.split_once('=')
            && name.trim().starts_with('@')
        {
            return Ok(Some(Statement::SetVariable {
                name: clean_variable(name),
                value: parse_user_literal(value.trim())?,
            }));
        }
    }
    if sql.len() >= 8 && sql[..8].eq_ignore_ascii_case("PREPARE ") {
        let rest = sql[8..].trim();
        let upper = rest.to_ascii_uppercase();
        let offset = upper
            .find(" FROM ")
            .ok_or_else(|| "PREPARE requires FROM".to_string())?;
        let name = clean_identifier(rest[..offset].trim());
        let source = rest[offset + 6..].trim();
        let source = if source.starts_with('@') {
            PrepareSource::Variable(clean_variable(source))
        } else {
            PrepareSource::Sql(parse_quoted_string(source)?)
        };
        return Ok(Some(Statement::Prepare { name, source }));
    }
    if sql.len() >= 8 && sql[..8].eq_ignore_ascii_case("EXECUTE ") {
        let rest = sql[8..].trim();
        let upper = rest.to_ascii_uppercase();
        let (name, using) = if let Some(offset) = upper.find(" USING ") {
            (
                &rest[..offset],
                rest[offset + 7..].split(',').map(clean_variable).collect(),
            )
        } else {
            (rest, Vec::new())
        };
        return Ok(Some(Statement::ExecutePrepared {
            name: clean_identifier(name),
            using,
        }));
    }
    for prefix in ["DEALLOCATE PREPARE ", "DROP PREPARE "] {
        if sql.len() >= prefix.len() && sql[..prefix.len()].eq_ignore_ascii_case(prefix) {
            return Ok(Some(Statement::DeallocatePrepared {
                name: clean_identifier(&sql[prefix.len()..]),
            }));
        }
    }
    if words.len() >= 3
        && words[0].eq_ignore_ascii_case("SHOW")
        && (words[1].eq_ignore_ascii_case("INDEX")
            || words[1].eq_ignore_ascii_case("INDEXES")
            || words[1].eq_ignore_ascii_case("KEYS"))
        && (words[2].eq_ignore_ascii_case("FROM") || words[2].eq_ignore_ascii_case("ON"))
    {
        let table_name = words
            .get(3)
            .ok_or_else(|| "SHOW INDEXES requires a table name".to_string())?;
        return Ok(Some(Statement::ShowIndexes {
            table_name: clean_identifier(table_name),
        }));
    }

    if words.len() >= 5
        && words[0].eq_ignore_ascii_case("DROP")
        && words[1].eq_ignore_ascii_case("INDEX")
    {
        let mut offset = 2;
        let if_exists = words
            .get(offset)
            .is_some_and(|word| word.eq_ignore_ascii_case("IF"))
            && words
                .get(offset + 1)
                .is_some_and(|word| word.eq_ignore_ascii_case("EXISTS"));
        if if_exists {
            offset += 2;
        }
        let name = words
            .get(offset)
            .ok_or_else(|| "DROP INDEX requires an index name".to_string())?;
        if !words
            .get(offset + 1)
            .is_some_and(|word| word.eq_ignore_ascii_case("ON"))
        {
            return Ok(None);
        }
        let table_name = words
            .get(offset + 2)
            .ok_or_else(|| "DROP INDEX requires ON <table>".to_string())?;
        return Ok(Some(Statement::DropIndex {
            name: clean_identifier(name),
            table_name: clean_identifier(table_name),
            if_exists,
        }));
    }
    Ok(None)
}

fn clean_identifier(value: &str) -> String {
    value
        .trim_matches(|character| matches!(character, '`' | '"' | '\'' | ';'))
        .to_ascii_lowercase()
}

fn object_name(name: &sql::ObjectName) -> String {
    name.0
        .last()
        .map(|identifier| identifier.value.to_ascii_lowercase())
        .unwrap_or_default()
}

fn convert_statement(statement: sql::Statement) -> Result<Statement, String> {
    match statement {
        sql::Statement::CreateTable(create) => convert_create_table(create),
        sql::Statement::CreateIndex(create) => {
            if create.using.is_some() || create.predicate.is_some() || !create.include.is_empty() {
                return Err("Partial, covering, and non-B-tree indexes are unsupported".to_string());
            }
            let name = create
                .name
                .as_ref()
                .map(object_name)
                .ok_or_else(|| "CREATE INDEX requires an index name".to_string())?;
            let columns = create
                .columns
                .into_iter()
                .map(|column| match column.expr {
                    sql::Expr::Identifier(identifier) => Ok(identifier.value.to_ascii_lowercase()),
                    _ => Err("Index expressions are unsupported".to_string()),
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Statement::CreateIndex {
                name,
                table_name: object_name(&create.table_name),
                columns,
                unique: create.unique,
                if_not_exists: create.if_not_exists,
            })
        }
        sql::Statement::AlterTable {
            name,
            if_exists,
            only,
            operations,
            location,
            on_cluster,
        } => {
            if if_exists || only || location.is_some() || on_cluster.is_some() {
                return Err(
                    "ALTER TABLE IF EXISTS/ONLY/location/cluster is unsupported".to_string()
                );
            }
            Ok(Statement::AlterTable {
                table_name: object_name(&name),
                operations: operations
                    .into_iter()
                    .map(convert_alter_operation)
                    .collect::<Result<Vec<_>, _>>()?,
            })
        }
        sql::Statement::Drop {
            object_type,
            if_exists,
            names,
            ..
        } => {
            if object_type != sql::ObjectType::Table || names.len() != 1 {
                return Err("Only DROP TABLE is supported by this form".to_string());
            }
            Ok(Statement::DropTable {
                table_name: object_name(&names[0]),
                if_exists,
            })
        }
        sql::Statement::Insert(insert) => convert_insert(insert),
        sql::Statement::Query(query) => Ok(Statement::Query(convert_query(*query)?)),
        sql::Statement::Update {
            table,
            assignments,
            selection,
            ..
        } => {
            if !table.joins.is_empty() {
                return Err("UPDATE with JOIN is unsupported".to_string());
            }
            let table_name = table_factor_name(&table.relation)?;
            let assignments = assignments
                .into_iter()
                .map(|assignment| {
                    let name = match assignment.target {
                        sql::AssignmentTarget::ColumnName(name) => object_name(&name),
                        sql::AssignmentTarget::Tuple(_) => {
                            return Err("Tuple assignment is unsupported".to_string());
                        }
                    };
                    Ok((name, convert_expr(assignment.value)?))
                })
                .collect::<Result<Vec<_>, String>>()?;
            Ok(Statement::Update {
                table_name,
                assignments,
                selection: selection.map(convert_expr).transpose()?,
            })
        }
        sql::Statement::Delete(delete) => {
            let tables = match delete.from {
                sql::FromTable::WithFromKeyword(tables)
                | sql::FromTable::WithoutKeyword(tables) => tables,
            };
            if tables.len() != 1 || !tables[0].joins.is_empty() {
                return Err("Only single-table DELETE is supported".to_string());
            }
            Ok(Statement::Delete {
                table_name: table_factor_name(&tables[0].relation)?,
                selection: delete.selection.map(convert_expr).transpose()?,
            })
        }
        sql::Statement::ShowTables { .. } => Ok(Statement::ShowTables),
        sql::Statement::ExplainTable { table_name, .. } => Ok(Statement::DescribeTable {
            table_name: object_name(&table_name),
        }),
        sql::Statement::Explain { statement, .. } => {
            Ok(Statement::Explain(Box::new(convert_statement(*statement)?)))
        }
        sql::Statement::StartTransaction { .. } => Ok(Statement::Begin),
        sql::Statement::Commit { .. } => Ok(Statement::Commit),
        sql::Statement::Rollback {
            savepoint: None, ..
        } => Ok(Statement::Rollback),
        other => Err(format!("Unsupported SQL statement: {other}")),
    }
}

fn convert_create_table(create: sql::CreateTable) -> Result<Statement, String> {
    if create.query.is_some() || create.like.is_some() || create.clone.is_some() {
        return Err("CREATE TABLE AS/LIKE/CLONE is unsupported".to_string());
    }
    let table_name = object_name(&create.name);
    let mut columns = Vec::with_capacity(create.columns.len());
    let mut constraints = Vec::new();

    for column in create.columns {
        let (converted, mut column_constraints) = convert_column_def(column)?;
        constraints.append(&mut column_constraints);
        columns.push(converted);
    }

    for constraint in create.constraints {
        constraints.push(convert_table_constraint(constraint)?);
    }

    Ok(Statement::CreateTable {
        table_name,
        columns,
        constraints,
        if_not_exists: create.if_not_exists,
    })
}

fn clean_variable(value: &str) -> String {
    value
        .trim()
        .trim_start_matches('@')
        .trim_end_matches(':')
        .trim()
        .to_ascii_lowercase()
}

fn parse_quoted_string(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.len() < 2 || !value.starts_with('\'') || !value.ends_with('\'') {
        return Err("Expected a single-quoted SQL string".to_string());
    }
    Ok(value[1..value.len() - 1].replace("''", "'"))
}

fn parse_user_literal(value: &str) -> Result<Value, String> {
    if value.eq_ignore_ascii_case("NULL") {
        Ok(Value::Null)
    } else if value.eq_ignore_ascii_case("TRUE") {
        Ok(Value::Boolean(true))
    } else if value.eq_ignore_ascii_case("FALSE") {
        Ok(Value::Boolean(false))
    } else if value.starts_with('\'') {
        parse_quoted_string(value).map(Value::Text)
    } else if value.contains(['.', 'e', 'E']) {
        value
            .parse::<f64>()
            .map(Value::Float)
            .map_err(|_| "SET user variables require a literal value".to_string())
    } else {
        value
            .parse::<i64>()
            .map(Value::Integer)
            .map_err(|_| "SET user variables require a literal value".to_string())
    }
}

pub fn parameter_count(statement: &Statement) -> usize {
    let mut statement = statement.clone();
    let mut next = 0;
    visit_statement_parameters(&mut statement, &mut |parameter| {
        next = next.max(parameter.saturating_add(1));
    });
    next
}

pub fn bind_parameters(statement: &Statement, values: &[Value]) -> Result<Statement, String> {
    let expected = parameter_count(statement);
    if values.len() != expected {
        return Err(format!(
            "Expected {expected} parameters, got {}",
            values.len()
        ));
    }
    let mut statement = statement.clone();
    visit_statement_exprs(&mut statement, &mut |expr| {
        if let Expr::Parameter(index) = expr {
            *expr = Expr::Literal(values[*index].clone());
        }
    });
    Ok(statement)
}

fn assign_parameters(statement: &mut Statement) {
    let mut next = 0;
    visit_statement_exprs(statement, &mut |expr| {
        if matches!(expr, Expr::Parameter(_)) {
            *expr = Expr::Parameter(next);
            next += 1;
        }
    });
}

fn visit_statement_parameters(statement: &mut Statement, visitor: &mut impl FnMut(usize)) {
    visit_statement_exprs(statement, &mut |expr| {
        if let Expr::Parameter(index) = expr {
            visitor(*index)
        }
    });
}

fn visit_statement_exprs(statement: &mut Statement, visitor: &mut impl FnMut(&mut Expr)) {
    match statement {
        Statement::CreateTable {
            columns,
            constraints,
            ..
        } => {
            for column in columns {
                for check in &mut column.checks {
                    visit_expr(check, visitor);
                }
            }
            for constraint in constraints {
                visit_constraint_exprs(constraint, visitor);
            }
        }
        Statement::AlterTable { operations, .. } => {
            for operation in operations {
                match operation {
                    AlterOperation::AddColumn {
                        column,
                        constraints,
                        ..
                    }
                    | AlterOperation::ModifyColumn {
                        column,
                        constraints,
                        ..
                    } => {
                        for check in &mut column.checks {
                            visit_expr(check, visitor);
                        }
                        for constraint in constraints {
                            visit_constraint_exprs(constraint, visitor);
                        }
                    }
                    AlterOperation::AddConstraint(constraint) => {
                        visit_constraint_exprs(constraint, visitor)
                    }
                    _ => {}
                }
            }
        }
        Statement::Insert {
            source,
            on_duplicate,
            ..
        } => {
            match source {
                InsertSource::Values(rows) => {
                    for row in rows {
                        for expr in row {
                            visit_expr(expr, visitor);
                        }
                    }
                }
                InsertSource::Query(query) => visit_query(query, visitor),
            }
            for (_, expr) in on_duplicate {
                visit_expr(expr, visitor);
            }
        }
        Statement::Query(query) => visit_query(query, visitor),
        Statement::Update {
            assignments,
            selection,
            ..
        } => {
            for (_, expr) in assignments {
                visit_expr(expr, visitor);
            }
            if let Some(expr) = selection {
                visit_expr(expr, visitor);
            }
        }
        Statement::Delete {
            selection: Some(expr),
            ..
        } => visit_expr(expr, visitor),
        Statement::Delete {
            selection: None, ..
        } => {}
        Statement::Explain(statement) => visit_statement_exprs(statement, visitor),
        _ => {}
    }
}

fn visit_constraint_exprs(constraint: &mut TableConstraint, visitor: &mut impl FnMut(&mut Expr)) {
    if let TableConstraint::Check { expr, .. } = constraint {
        visit_expr(expr, visitor);
    }
}

fn visit_query(query: &mut Query, visitor: &mut impl FnMut(&mut Expr)) {
    for cte in &mut query.ctes {
        visit_query(&mut cte.query, visitor);
    }
    for item in &mut query.projection {
        if let SelectItem::Expr { expr, .. } = item {
            visit_expr(expr, visitor);
        }
    }
    if let Some(source) = &mut query.from {
        visit_source(source, visitor);
    }
    for join in &mut query.joins {
        visit_source(&mut join.source, visitor);
        if let Some(expr) = &mut join.on {
            visit_expr(expr, visitor);
        }
    }
    if let Some(expr) = &mut query.selection {
        visit_expr(expr, visitor);
    }
    for expr in &mut query.group_by {
        visit_expr(expr, visitor);
    }
    if let Some(expr) = &mut query.having {
        visit_expr(expr, visitor);
    }
    for order in &mut query.order_by {
        visit_expr(&mut order.expr, visitor);
    }
}

fn visit_source(source: &mut TableSource, visitor: &mut impl FnMut(&mut Expr)) {
    if let TableSource::Derived { query, .. } = source {
        visit_query(query, visitor);
    }
}

fn visit_expr(expr: &mut Expr, visitor: &mut impl FnMut(&mut Expr)) {
    visitor(expr);
    match expr {
        Expr::Binary { left, right, .. } => {
            visit_expr(left, visitor);
            visit_expr(right, visitor);
        }
        Expr::Unary { expr, .. } | Expr::IsNull { expr, .. } => visit_expr(expr, visitor),
        Expr::Like { expr, pattern, .. } => {
            visit_expr(expr, visitor);
            visit_expr(pattern, visitor);
        }
        Expr::InList { expr, list, .. } => {
            visit_expr(expr, visitor);
            for item in list {
                visit_expr(item, visitor);
            }
        }
        Expr::InSubquery { expr, query, .. } => {
            visit_expr(expr, visitor);
            visit_query(query, visitor);
        }
        Expr::Exists { query, .. } | Expr::ScalarSubquery(query) => visit_query(query, visitor),
        Expr::Aggregate {
            expr: Some(expr), ..
        } => visit_expr(expr, visitor),
        _ => {}
    }
}

fn convert_column_def(column: sql::ColumnDef) -> Result<(ColumnDef, Vec<TableConstraint>), String> {
    let mut converted = ColumnDef::new(
        &column.name.value.to_ascii_lowercase(),
        convert_data_type(&column.data_type)?,
    );
    let mut constraints = Vec::new();
    for option in column.options {
        let constraint_name = option.name.map(|name| name.value.to_ascii_lowercase());
        match option.option {
            sql::ColumnOption::Null => converted.nullable = true,
            sql::ColumnOption::NotNull => converted.nullable = false,
            sql::ColumnOption::Default(expr) => converted.default = Some(literal_value(expr)?),
            sql::ColumnOption::Unique { is_primary, .. } => {
                if is_primary {
                    converted = converted.primary_key();
                } else {
                    converted = converted.unique();
                }
            }
            sql::ColumnOption::Check(expr) => {
                let expr = convert_expr(expr)?;
                converted.checks.push(expr.clone());
                constraints.push(TableConstraint::Check {
                    name: constraint_name,
                    expr,
                });
            }
            sql::ColumnOption::ForeignKey {
                foreign_table,
                referred_columns,
                on_delete,
                on_update,
                ..
            } => {
                validate_referential_actions(on_delete, on_update)?;
                constraints.push(TableConstraint::ForeignKey(ForeignKeyConstraint {
                    name: constraint_name,
                    columns: vec![converted.name.clone()],
                    foreign_table: object_name(&foreign_table),
                    referred_columns: referred_columns
                        .into_iter()
                        .map(|column| column.value.to_ascii_lowercase())
                        .collect(),
                }));
            }
            unsupported => return Err(format!("Unsupported column option: {unsupported}")),
        }
    }
    if converted.primary_key {
        constraints.push(TableConstraint::PrimaryKey {
            name: None,
            columns: vec![converted.name.clone()],
        });
    } else if converted.unique {
        constraints.push(TableConstraint::Unique {
            name: None,
            columns: vec![converted.name.clone()],
        });
    }
    Ok((converted, constraints))
}

fn convert_alter_operation(operation: sql::AlterTableOperation) -> Result<AlterOperation, String> {
    use sql::AlterColumnOperation as ColumnOp;
    use sql::AlterTableOperation as Op;
    match operation {
        Op::AddColumn {
            column_def,
            if_not_exists,
            column_position,
            ..
        } => {
            if column_position.is_some() {
                return Err("ALTER TABLE column positioning is unsupported".to_string());
            }
            let (column, constraints) = convert_column_def(column_def)?;
            Ok(AlterOperation::AddColumn {
                column,
                constraints,
                if_not_exists,
            })
        }
        Op::DropColumn {
            column_name,
            if_exists,
            cascade,
        } => {
            if cascade {
                return Err("ALTER TABLE DROP COLUMN CASCADE is unsupported".to_string());
            }
            Ok(AlterOperation::DropColumn {
                name: column_name.value.to_ascii_lowercase(),
                if_exists,
            })
        }
        Op::RenameColumn {
            old_column_name,
            new_column_name,
        } => Ok(AlterOperation::RenameColumn {
            old_name: old_column_name.value.to_ascii_lowercase(),
            new_name: new_column_name.value.to_ascii_lowercase(),
        }),
        Op::ChangeColumn {
            old_name,
            new_name,
            data_type,
            options,
            column_position,
        } => {
            if column_position.is_some() {
                return Err("ALTER TABLE column positioning is unsupported".to_string());
            }
            let (column, constraints) = convert_column_def(sql::ColumnDef {
                name: new_name,
                data_type,
                collation: None,
                options: options
                    .into_iter()
                    .map(|option| sql::ColumnOptionDef { name: None, option })
                    .collect(),
            })?;
            Ok(AlterOperation::ModifyColumn {
                old_name: old_name.value.to_ascii_lowercase(),
                column,
                constraints,
            })
        }
        Op::ModifyColumn {
            col_name,
            data_type,
            options,
            column_position,
        } => {
            if column_position.is_some() {
                return Err("ALTER TABLE column positioning is unsupported".to_string());
            }
            let old_name = col_name.value.to_ascii_lowercase();
            let (column, constraints) = convert_column_def(sql::ColumnDef {
                name: col_name,
                data_type,
                collation: None,
                options: options
                    .into_iter()
                    .map(|option| sql::ColumnOptionDef { name: None, option })
                    .collect(),
            })?;
            Ok(AlterOperation::ModifyColumn {
                old_name,
                column,
                constraints,
            })
        }
        Op::AlterColumn { column_name, op } => match op {
            ColumnOp::SetNotNull => Ok(AlterOperation::SetNullable {
                column: clean_identifier(&column_name.value),
                nullable: false,
            }),
            ColumnOp::DropNotNull => Ok(AlterOperation::SetNullable {
                column: clean_identifier(&column_name.value),
                nullable: true,
            }),
            ColumnOp::SetDefault { value } => Ok(AlterOperation::SetDefault {
                column: clean_identifier(&column_name.value),
                value: Some(literal_value(value)?),
            }),
            ColumnOp::DropDefault => Ok(AlterOperation::SetDefault {
                column: clean_identifier(&column_name.value),
                value: None,
            }),
            ColumnOp::SetDataType {
                data_type,
                using: None,
            } => Ok(AlterOperation::SetDataType {
                column: clean_identifier(&column_name.value),
                data_type: convert_data_type(&data_type)?,
            }),
            _ => Err("Unsupported ALTER COLUMN operation".to_string()),
        },
        Op::AddConstraint(constraint) => Ok(AlterOperation::AddConstraint(
            convert_table_constraint(constraint)?,
        )),
        Op::DropConstraint {
            if_exists,
            name,
            cascade,
        } => {
            if cascade {
                return Err("ALTER TABLE DROP CONSTRAINT CASCADE is unsupported".to_string());
            }
            Ok(AlterOperation::DropConstraint {
                name: name.value.to_ascii_lowercase(),
                if_exists,
            })
        }
        Op::DropPrimaryKey => Ok(AlterOperation::DropPrimaryKey),
        other => Err(format!("Unsupported ALTER TABLE operation: {other}")),
    }
}

fn convert_table_constraint(constraint: sql::TableConstraint) -> Result<TableConstraint, String> {
    match constraint {
        sql::TableConstraint::PrimaryKey { name, columns, .. } => Ok(TableConstraint::PrimaryKey {
            name: name.map(|name| name.value.to_ascii_lowercase()),
            columns: columns
                .into_iter()
                .map(|column| column.value.to_ascii_lowercase())
                .collect(),
        }),
        sql::TableConstraint::Unique { name, columns, .. } => Ok(TableConstraint::Unique {
            name: name.map(|name| name.value.to_ascii_lowercase()),
            columns: columns
                .into_iter()
                .map(|column| column.value.to_ascii_lowercase())
                .collect(),
        }),
        sql::TableConstraint::Check { name, expr } => Ok(TableConstraint::Check {
            name: name.map(|name| name.value.to_ascii_lowercase()),
            expr: convert_expr(*expr)?,
        }),
        sql::TableConstraint::ForeignKey {
            name,
            columns,
            foreign_table,
            referred_columns,
            on_delete,
            on_update,
            ..
        } => {
            validate_referential_actions(on_delete, on_update)?;
            Ok(TableConstraint::ForeignKey(ForeignKeyConstraint {
                name: name.map(|name| name.value.to_ascii_lowercase()),
                columns: columns
                    .into_iter()
                    .map(|column| column.value.to_ascii_lowercase())
                    .collect(),
                foreign_table: object_name(&foreign_table),
                referred_columns: referred_columns
                    .into_iter()
                    .map(|column| column.value.to_ascii_lowercase())
                    .collect(),
            }))
        }
        sql::TableConstraint::Index { .. } => Err(
            "Inline INDEX declarations are unsupported; use CREATE INDEX after CREATE TABLE"
                .to_string(),
        ),
        other => Err(format!("Unsupported table constraint: {other}")),
    }
}

fn validate_referential_actions(
    on_delete: Option<sql::ReferentialAction>,
    on_update: Option<sql::ReferentialAction>,
) -> Result<(), String> {
    for action in [on_delete, on_update].into_iter().flatten() {
        if !matches!(
            action,
            sql::ReferentialAction::Restrict | sql::ReferentialAction::NoAction
        ) {
            return Err("Only RESTRICT/NO ACTION foreign keys are supported".to_string());
        }
    }
    Ok(())
}

fn convert_insert(insert: sql::Insert) -> Result<Statement, String> {
    if insert.ignore || insert.replace_into {
        return Err("INSERT IGNORE/REPLACE clauses are unsupported".to_string());
    }
    let source = insert
        .source
        .ok_or_else(|| "INSERT requires VALUES".to_string())?;
    let source = match *source.body {
        sql::SetExpr::Values(values) => InsertSource::Values(
            values
                .rows
                .into_iter()
                .map(|row| row.into_iter().map(convert_expr).collect())
                .collect::<Result<Vec<_>, _>>()?,
        ),
        _ => InsertSource::Query(Box::new(convert_query(*source)?)),
    };
    let on_duplicate = match insert.on {
        None => Vec::new(),
        Some(sql::OnInsert::DuplicateKeyUpdate(assignments)) => assignments
            .into_iter()
            .map(|assignment| {
                let name = match assignment.target {
                    sql::AssignmentTarget::ColumnName(name) => object_name(&name),
                    sql::AssignmentTarget::Tuple(_) => {
                        return Err("Tuple assignment is unsupported".to_string());
                    }
                };
                Ok((name, convert_expr(assignment.value)?))
            })
            .collect::<Result<Vec<_>, String>>()?,
        Some(other) => return Err(format!("Unsupported INSERT conflict clause: {other}")),
    };
    Ok(Statement::Insert {
        table_name: object_name(&insert.table_name),
        columns: if insert.columns.is_empty() {
            None
        } else {
            Some(
                insert
                    .columns
                    .into_iter()
                    .map(|column| column.value.to_ascii_lowercase())
                    .collect(),
            )
        },
        source,
        on_duplicate,
    })
}

fn convert_query(query: sql::Query) -> Result<Query, String> {
    if query.offset.is_some() || query.fetch.is_some() || !query.limit_by.is_empty() {
        return Err("OFFSET/FETCH/LIMIT BY are unsupported".to_string());
    }
    let ctes = if let Some(with) = query.with {
        if with.recursive {
            return Err("Recursive CTEs are unsupported".to_string());
        }
        with.cte_tables
            .into_iter()
            .map(|cte| {
                Ok(Cte {
                    name: cte.alias.name.value.to_ascii_lowercase(),
                    columns: cte
                        .alias
                        .columns
                        .into_iter()
                        .map(|column| column.name.value.to_ascii_lowercase())
                        .collect(),
                    query: Box::new(convert_query(*cte.query)?),
                })
            })
            .collect::<Result<Vec<_>, String>>()?
    } else {
        Vec::new()
    };

    let select = match *query.body {
        sql::SetExpr::Select(select) => *select,
        sql::SetExpr::Query(query) => return convert_query(*query),
        _ => return Err("Set operations are unsupported".to_string()),
    };
    if select.from.len() > 1 {
        return Err("Comma joins are unsupported; use explicit JOIN syntax".to_string());
    }
    let (from, joins) = if let Some(table) = select.from.into_iter().next() {
        let from = Some(convert_table_source(table.relation)?);
        let joins = table
            .joins
            .into_iter()
            .map(convert_join)
            .collect::<Result<Vec<_>, _>>()?;
        (from, joins)
    } else {
        (None, Vec::new())
    };
    let group_by = match select.group_by {
        sql::GroupByExpr::Expressions(expressions, modifiers) if modifiers.is_empty() => {
            expressions
                .into_iter()
                .map(convert_expr)
                .collect::<Result<Vec<_>, _>>()?
        }
        sql::GroupByExpr::Expressions(_, _) | sql::GroupByExpr::All(_) => {
            return Err("GROUP BY modifiers/ALL are unsupported".to_string());
        }
    };
    let order_by = query
        .order_by
        .map(|order| {
            order
                .exprs
                .into_iter()
                .map(|item| {
                    Ok(OrderBy {
                        expr: convert_expr(item.expr)?,
                        descending: item.asc == Some(false),
                    })
                })
                .collect::<Result<Vec<_>, String>>()
        })
        .transpose()?
        .unwrap_or_default();
    let limit = query.limit.map(literal_usize).transpose()?;

    Ok(Query {
        ctes,
        distinct: select.distinct.is_some(),
        projection: select
            .projection
            .into_iter()
            .map(convert_select_item)
            .collect::<Result<Vec<_>, _>>()?,
        from,
        joins,
        selection: select.selection.map(convert_expr).transpose()?,
        group_by,
        having: select.having.map(convert_expr).transpose()?,
        order_by,
        limit,
    })
}

fn convert_select_item(item: sql::SelectItem) -> Result<SelectItem, String> {
    match item {
        sql::SelectItem::Wildcard(options) if options == Default::default() => {
            Ok(SelectItem::Wildcard(None))
        }
        sql::SelectItem::QualifiedWildcard(name, options) if options == Default::default() => {
            Ok(SelectItem::Wildcard(Some(object_name(&name))))
        }
        sql::SelectItem::UnnamedExpr(expr) => Ok(SelectItem::Expr {
            expr: convert_expr(expr)?,
            alias: None,
        }),
        sql::SelectItem::ExprWithAlias { expr, alias } => Ok(SelectItem::Expr {
            expr: convert_expr(expr)?,
            alias: Some(alias.value.to_ascii_lowercase()),
        }),
        _ => Err("Wildcard modifiers are unsupported".to_string()),
    }
}

fn convert_table_source(source: sql::TableFactor) -> Result<TableSource, String> {
    match source {
        sql::TableFactor::Table {
            name, alias, args, ..
        } if args.is_none() => Ok(TableSource::Table {
            name: object_name(&name),
            alias: alias.map(|alias| alias.name.value.to_ascii_lowercase()),
        }),
        sql::TableFactor::Derived {
            lateral: false,
            subquery,
            alias: Some(alias),
        } => Ok(TableSource::Derived {
            query: Box::new(convert_query(*subquery)?),
            alias: alias.name.value.to_ascii_lowercase(),
        }),
        sql::TableFactor::Derived { lateral: true, .. } => {
            Err("LATERAL subqueries are unsupported".to_string())
        }
        sql::TableFactor::Derived { alias: None, .. } => {
            Err("Derived tables require an alias".to_string())
        }
        _ => Err("Unsupported table source".to_string()),
    }
}

fn table_factor_name(source: &sql::TableFactor) -> Result<String, String> {
    match source {
        sql::TableFactor::Table { name, args, .. } if args.is_none() => Ok(object_name(name)),
        _ => Err("Expected a table name".to_string()),
    }
}

fn convert_join(join: sql::Join) -> Result<Join, String> {
    let (join_type, constraint) = match join.join_operator {
        sql::JoinOperator::Inner(constraint) => (JoinType::Inner, Some(constraint)),
        sql::JoinOperator::LeftOuter(constraint) => (JoinType::Left, Some(constraint)),
        sql::JoinOperator::RightOuter(constraint) => (JoinType::Right, Some(constraint)),
        sql::JoinOperator::CrossJoin => (JoinType::Cross, None),
        sql::JoinOperator::FullOuter(_) => return Err("FULL JOIN is unsupported".to_string()),
        _ => return Err("Unsupported JOIN type".to_string()),
    };
    let on = match constraint {
        None | Some(sql::JoinConstraint::None) => None,
        Some(sql::JoinConstraint::On(expr)) => Some(convert_expr(expr)?),
        Some(sql::JoinConstraint::Using(_)) | Some(sql::JoinConstraint::Natural) => {
            return Err("NATURAL/USING joins are unsupported; use ON".to_string());
        }
    };
    Ok(Join {
        join_type,
        source: convert_table_source(join.relation)?,
        on,
    })
}

fn convert_expr(expr: sql::Expr) -> Result<Expr, String> {
    match expr {
        sql::Expr::Identifier(identifier) => Ok(Expr::Column {
            qualifier: None,
            name: identifier.value.to_ascii_lowercase(),
        }),
        sql::Expr::CompoundIdentifier(identifiers) if identifiers.len() == 2 => Ok(Expr::Column {
            qualifier: Some(identifiers[0].value.to_ascii_lowercase()),
            name: identifiers[1].value.to_ascii_lowercase(),
        }),
        sql::Expr::Value(sql::Value::Placeholder(_)) => Ok(Expr::Parameter(usize::MAX)),
        sql::Expr::Value(value) => Ok(Expr::Literal(convert_value(value)?)),
        sql::Expr::Nested(expr) => convert_expr(*expr),
        sql::Expr::BinaryOp { left, op, right } => Ok(Expr::Binary {
            left: Box::new(convert_expr(*left)?),
            op: convert_binary_op(op)?,
            right: Box::new(convert_expr(*right)?),
        }),
        sql::Expr::UnaryOp { op, expr } => Ok(Expr::Unary {
            op: match op {
                sql::UnaryOperator::Not => UnaryOp::Not,
                sql::UnaryOperator::Minus => UnaryOp::Negate,
                sql::UnaryOperator::Plus => UnaryOp::Plus,
                _ => return Err(format!("Unsupported unary operator: {op}")),
            },
            expr: Box::new(convert_expr(*expr)?),
        }),
        sql::Expr::IsNull(expr) => Ok(Expr::IsNull {
            expr: Box::new(convert_expr(*expr)?),
            negated: false,
        }),
        sql::Expr::IsNotNull(expr) => Ok(Expr::IsNull {
            expr: Box::new(convert_expr(*expr)?),
            negated: true,
        }),
        sql::Expr::Like {
            negated,
            any: false,
            expr,
            pattern,
            escape_char: None,
        } => Ok(Expr::Like {
            expr: Box::new(convert_expr(*expr)?),
            pattern: Box::new(convert_expr(*pattern)?),
            negated,
        }),
        sql::Expr::InList {
            expr,
            list,
            negated,
        } => Ok(Expr::InList {
            expr: Box::new(convert_expr(*expr)?),
            list: list
                .into_iter()
                .map(convert_expr)
                .collect::<Result<Vec<_>, _>>()?,
            negated,
        }),
        sql::Expr::InSubquery {
            expr,
            subquery,
            negated,
        } => Ok(Expr::InSubquery {
            expr: Box::new(convert_expr(*expr)?),
            query: Box::new(convert_query(*subquery)?),
            negated,
        }),
        sql::Expr::Exists { subquery, negated } => Ok(Expr::Exists {
            query: Box::new(convert_query(*subquery)?),
            negated,
        }),
        sql::Expr::Subquery(query) => Ok(Expr::ScalarSubquery(Box::new(convert_query(*query)?))),
        sql::Expr::Function(function) => convert_function(function),
        other => Err(format!("Unsupported expression: {other}")),
    }
}

fn convert_function(function: sql::Function) -> Result<Expr, String> {
    if function.over.is_some() || function.filter.is_some() || !function.within_group.is_empty() {
        return Err("Window/FILTER/WITHIN GROUP functions are unsupported".to_string());
    }
    let function_name = object_name(&function.name);
    if function_name == "values" {
        let arguments = match function.args {
            sql::FunctionArguments::List(arguments) => arguments.args,
            _ => return Err("VALUES() requires one column argument".to_string()),
        };
        return match arguments.as_slice() {
            [
                sql::FunctionArg::Unnamed(sql::FunctionArgExpr::Expr(sql::Expr::Identifier(
                    column,
                ))),
            ] => Ok(Expr::Incoming(column.value.to_ascii_lowercase())),
            _ => Err("VALUES() requires one column argument".to_string()),
        };
    }
    let aggregate = match function_name.as_str() {
        "count" => AggregateFunction::Count,
        "sum" => AggregateFunction::Sum,
        "avg" => AggregateFunction::Avg,
        "max" => AggregateFunction::Max,
        "min" => AggregateFunction::Min,
        name => return Err(format!("Unsupported function: {name}")),
    };
    let (arguments, distinct) = match function.args {
        sql::FunctionArguments::List(arguments) => (
            arguments.args,
            arguments.duplicate_treatment == Some(sql::DuplicateTreatment::Distinct),
        ),
        _ => return Err("Aggregate functions require a normal argument list".to_string()),
    };
    if arguments.len() != 1 {
        return Err("Aggregate functions require exactly one argument".to_string());
    }
    let expr = match arguments.into_iter().next().unwrap() {
        sql::FunctionArg::Unnamed(sql::FunctionArgExpr::Wildcard) => None,
        sql::FunctionArg::Unnamed(sql::FunctionArgExpr::Expr(expr)) => {
            Some(Box::new(convert_expr(expr)?))
        }
        _ => return Err("Unsupported aggregate argument".to_string()),
    };
    if expr.is_none() && aggregate != AggregateFunction::Count {
        return Err("Only COUNT supports '*'".to_string());
    }
    Ok(Expr::Aggregate {
        function: aggregate,
        expr,
        distinct,
    })
}

fn convert_binary_op(op: sql::BinaryOperator) -> Result<BinaryOp, String> {
    Ok(match op {
        sql::BinaryOperator::Plus => BinaryOp::Add,
        sql::BinaryOperator::Minus => BinaryOp::Subtract,
        sql::BinaryOperator::Multiply => BinaryOp::Multiply,
        sql::BinaryOperator::Divide => BinaryOp::Divide,
        sql::BinaryOperator::Eq => BinaryOp::Compare(ComparisonOp::Equal),
        sql::BinaryOperator::NotEq => BinaryOp::Compare(ComparisonOp::NotEqual),
        sql::BinaryOperator::Lt => BinaryOp::Compare(ComparisonOp::LessThan),
        sql::BinaryOperator::LtEq => BinaryOp::Compare(ComparisonOp::LessThanOrEqual),
        sql::BinaryOperator::Gt => BinaryOp::Compare(ComparisonOp::GreaterThan),
        sql::BinaryOperator::GtEq => BinaryOp::Compare(ComparisonOp::GreaterThanOrEqual),
        sql::BinaryOperator::And => BinaryOp::And,
        sql::BinaryOperator::Or => BinaryOp::Or,
        other => return Err(format!("Unsupported binary operator: {other}")),
    })
}

fn convert_value(value: sql::Value) -> Result<Value, String> {
    match value {
        sql::Value::Number(number, _) => {
            if number.contains(['.', 'e', 'E']) {
                number
                    .parse()
                    .map(Value::Float)
                    .map_err(|_| format!("Invalid number: {number}"))
            } else {
                number
                    .parse()
                    .map(Value::Integer)
                    .map_err(|_| format!("Invalid integer: {number}"))
            }
        }
        sql::Value::SingleQuotedString(value)
        | sql::Value::DoubleQuotedString(value)
        | sql::Value::NationalStringLiteral(value)
        | sql::Value::EscapedStringLiteral(value) => Ok(Value::Text(value)),
        sql::Value::Boolean(value) => Ok(Value::Boolean(value)),
        sql::Value::Null => Ok(Value::Null),
        other => Err(format!("Unsupported literal: {other}")),
    }
}

fn literal_value(expr: sql::Expr) -> Result<Value, String> {
    match convert_expr(expr)? {
        Expr::Literal(value) => Ok(value),
        _ => Err("DEFAULT must be a literal".to_string()),
    }
}

fn literal_usize(expr: sql::Expr) -> Result<usize, String> {
    match convert_expr(expr)? {
        Expr::Literal(Value::Integer(value)) if value >= 0 => Ok(value as usize),
        _ => Err("LIMIT must be a non-negative integer".to_string()),
    }
}

fn convert_data_type(data_type: &sql::DataType) -> Result<DataType, String> {
    let text = data_type.to_string().to_ascii_uppercase();
    if text.starts_with("INT")
        || text.starts_with("INTEGER")
        || text.starts_with("BIGINT")
        || text.starts_with("SMALLINT")
    {
        Ok(DataType::Integer)
    } else if text.starts_with("FLOAT")
        || text.starts_with("DOUBLE")
        || text.starts_with("REAL")
        || text.starts_with("DECIMAL")
        || text.starts_with("NUMERIC")
    {
        Ok(DataType::Float)
    } else if text.starts_with("TEXT")
        || text.starts_with("VARCHAR")
        || text.starts_with("CHAR")
        || text.starts_with("STRING")
    {
        Ok(DataType::Text)
    } else if text.starts_with("BOOL") {
        Ok(DataType::Boolean)
    } else {
        Err(format!("Unsupported data type: {data_type}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_create_table_constraints() {
        let statement = parse_sql(
            "CREATE TABLE child (
                id INT PRIMARY KEY,
                parent_id INT,
                score FLOAT CHECK (score >= 0),
                UNIQUE (parent_id, score),
                FOREIGN KEY (parent_id) REFERENCES parent(id)
            )",
        )
        .unwrap();
        match statement {
            Statement::CreateTable {
                columns,
                constraints,
                ..
            } => {
                assert_eq!(columns.len(), 3);
                assert!(constraints.len() >= 4);
            }
            _ => panic!("expected create table"),
        }
    }

    #[test]
    fn parses_advanced_query() {
        let statement = parse_sql(
            "WITH totals AS (
                SELECT user_id, SUM(amount) AS total
                FROM orders GROUP BY user_id
            )
            SELECT u.name, t.total
            FROM users u LEFT JOIN totals t ON u.id = t.user_id
            WHERE u.id IN (SELECT user_id FROM orders)
            ORDER BY t.total DESC LIMIT 10",
        )
        .unwrap();
        match statement {
            Statement::Query(query) => {
                assert_eq!(query.ctes.len(), 1);
                assert_eq!(query.joins.len(), 1);
                assert_eq!(query.limit, Some(10));
            }
            _ => panic!("expected query"),
        }
    }

    #[test]
    fn parses_indexes_and_transactions() {
        assert!(matches!(
            parse_sql("CREATE UNIQUE INDEX idx_name ON users(name)").unwrap(),
            Statement::CreateIndex { unique: true, .. }
        ));
        assert!(matches!(
            parse_sql("DROP INDEX idx_name ON users").unwrap(),
            Statement::DropIndex { .. }
        ));
        assert!(matches!(parse_sql("BEGIN").unwrap(), Statement::Begin));
    }

    #[test]
    fn parses_migrations_upserts_and_prepared_parameters() {
        assert!(matches!(
            parse_sql("ALTER TABLE users ADD COLUMN active BOOLEAN NOT NULL DEFAULT TRUE").unwrap(),
            Statement::AlterTable { .. }
        ));
        assert!(
            matches!(parse_sql("ALTER TABLE child DROP FOREIGN KEY fk_parent").unwrap(), Statement::AlterTable { operations, .. } if matches!(&operations[0], AlterOperation::DropConstraint { name, .. } if name == "fk_parent"))
        );
        let statement = parse_sql("INSERT INTO users SELECT ?, name FROM source ON DUPLICATE KEY UPDATE name = VALUES(name)").unwrap();
        assert_eq!(parameter_count(&statement), 1);
        assert!(bind_parameters(&statement, &[Value::Integer(7)]).is_ok());
        assert!(matches!(
            parse_sql("PREPARE lookup FROM 'SELECT * FROM users WHERE id = ?'").unwrap(),
            Statement::Prepare { .. }
        ));
    }
}
