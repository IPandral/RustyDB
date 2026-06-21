use sqlparser::ast as sql;
use sqlparser::dialect::MySqlDialect;
use sqlparser::parser::Parser;

use super::types::*;

/// Parsed RustyDB statement. The public AST is independent of sqlparser so the
/// execution and persistence layers are not coupled to a third-party AST.
#[derive(Debug, Clone, PartialEq)]
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
    Insert {
        table_name: String,
        columns: Option<Vec<String>>,
        values: Vec<Vec<Expr>>,
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
    Begin,
    Commit,
    Rollback,
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
    convert_statement(statements.remove(0))
}

fn parse_rustydb_extension(sql: &str) -> Result<Option<Statement>, String> {
    let words: Vec<&str> = sql.split_whitespace().collect();
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
        let mut converted = ColumnDef::new(
            &column.name.value.to_ascii_lowercase(),
            convert_data_type(&column.data_type)?,
        );
        for option in column.options {
            let constraint_name = option.name.map(|name| name.value.to_ascii_lowercase());
            match option.option {
                sql::ColumnOption::Null => converted.nullable = true,
                sql::ColumnOption::NotNull => converted.nullable = false,
                sql::ColumnOption::Default(expr) => {
                    converted.default = Some(literal_value(expr)?);
                }
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
                unsupported => {
                    return Err(format!("Unsupported column option: {unsupported}"));
                }
            }
        }
        columns.push(converted);
    }

    for constraint in create.constraints {
        constraints.push(convert_table_constraint(constraint)?);
    }

    // Normalize column-level primary and unique declarations into table constraints.
    for column in &columns {
        if column.primary_key {
            constraints.push(TableConstraint::PrimaryKey {
                name: None,
                columns: vec![column.name.clone()],
            });
        } else if column.unique {
            constraints.push(TableConstraint::Unique {
                name: None,
                columns: vec![column.name.clone()],
            });
        }
    }

    Ok(Statement::CreateTable {
        table_name,
        columns,
        constraints,
        if_not_exists: create.if_not_exists,
    })
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
    if insert.on.is_some() || insert.ignore || insert.replace_into {
        return Err("INSERT conflict/replace clauses are unsupported".to_string());
    }
    let source = insert
        .source
        .ok_or_else(|| "INSERT requires VALUES".to_string())?;
    let rows = match *source.body {
        sql::SetExpr::Values(values) => values.rows,
        _ => return Err("INSERT ... SELECT is unsupported".to_string()),
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
        values: rows
            .into_iter()
            .map(|row| row.into_iter().map(convert_expr).collect())
            .collect::<Result<Vec<_>, _>>()?,
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
    let aggregate = match object_name(&function.name).as_str() {
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
}
