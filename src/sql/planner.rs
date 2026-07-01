use super::executor::Catalog;
use super::parser::Statement;
use super::types::*;

/// Builds a compact logical plan and applies RustyDB's rule-based choices.
pub struct Planner<'a> {
    catalog: &'a Catalog,
}

/// Stateless rule optimizer used before plans enter the LRU cache.
pub struct Optimizer;

impl Optimizer {
    pub fn optimize_statement(statement: Statement) -> Statement {
        match statement {
            Statement::Query(query) => Statement::Query(Self::optimize_query(query)),
            Statement::Insert {
                table_name,
                columns,
                values,
            } => Statement::Insert {
                table_name,
                columns,
                values: values
                    .into_iter()
                    .map(|row| row.into_iter().map(fold_expr).collect())
                    .collect(),
            },
            Statement::Update {
                table_name,
                assignments,
                selection,
            } => Statement::Update {
                table_name,
                assignments: assignments
                    .into_iter()
                    .map(|(column, expression)| (column, fold_expr(expression)))
                    .collect(),
                selection: selection.map(fold_expr),
            },
            Statement::Delete {
                table_name,
                selection,
            } => Statement::Delete {
                table_name,
                selection: selection.map(fold_expr),
            },
            Statement::Explain(statement) => {
                Statement::Explain(Box::new(Self::optimize_statement(*statement)))
            }
            other => other,
        }
    }

    pub fn optimize_query(mut query: Query) -> Query {
        query.ctes = query
            .ctes
            .into_iter()
            .map(|mut cte| {
                cte.query = Box::new(Self::optimize_query(*cte.query));
                cte
            })
            .collect();
        query.projection = query
            .projection
            .into_iter()
            .map(|item| match item {
                SelectItem::Expr { expr, alias } => SelectItem::Expr {
                    expr: fold_expr(expr),
                    alias,
                },
                wildcard => wildcard,
            })
            .collect();
        query.from = query.from.map(optimize_source);
        query.joins = query
            .joins
            .into_iter()
            .map(|mut join| {
                join.source = optimize_source(join.source);
                join.on = join.on.map(fold_expr);
                join
            })
            .collect();
        query.selection = query.selection.map(fold_expr);
        query.group_by = query.group_by.into_iter().map(fold_expr).collect();
        query.having = query.having.map(fold_expr);
        query.order_by = query
            .order_by
            .into_iter()
            .map(|mut order| {
                order.expr = fold_expr(order.expr);
                order
            })
            .collect();
        query
    }
}

fn optimize_source(source: TableSource) -> TableSource {
    match source {
        TableSource::Derived { query, alias } => TableSource::Derived {
            query: Box::new(Optimizer::optimize_query(*query)),
            alias,
        },
        table => table,
    }
}

fn fold_expr(expression: Expr) -> Expr {
    match expression {
        Expr::Binary { left, op, right } => {
            let left = fold_expr(*left);
            let right = fold_expr(*right);
            if let (Expr::Literal(left_value), Expr::Literal(right_value)) = (&left, &right)
                && let Some(value) = fold_binary(left_value, &op, right_value)
            {
                return Expr::Literal(value);
            }
            Expr::Binary {
                left: Box::new(left),
                op,
                right: Box::new(right),
            }
        }
        Expr::Unary { op, expr } => {
            let expr = fold_expr(*expr);
            if let Expr::Literal(value) = &expr {
                let folded = match (op.clone(), value) {
                    (UnaryOp::Not, value) => Some(truth_value(value.truth().negate())),
                    (UnaryOp::Negate, Value::Integer(value)) => Some(Value::Integer(-value)),
                    (UnaryOp::Negate, Value::Float(value)) => Some(Value::Float(-value)),
                    (UnaryOp::Plus, Value::Integer(value)) => Some(Value::Integer(*value)),
                    (UnaryOp::Plus, Value::Float(value)) => Some(Value::Float(*value)),
                    _ => None,
                };
                if let Some(value) = folded {
                    return Expr::Literal(value);
                }
            }
            Expr::Unary {
                op,
                expr: Box::new(expr),
            }
        }
        Expr::IsNull { expr, negated } => {
            let expr = fold_expr(*expr);
            if let Expr::Literal(value) = &expr {
                return Expr::Literal(Value::Boolean(if negated {
                    !value.is_null()
                } else {
                    value.is_null()
                }));
            }
            Expr::IsNull {
                expr: Box::new(expr),
                negated,
            }
        }
        Expr::Like {
            expr,
            pattern,
            negated,
        } => {
            let expr = fold_expr(*expr);
            let pattern = fold_expr(*pattern);
            if let (Expr::Literal(value), Expr::Literal(pattern)) = (&expr, &pattern) {
                let truth = value.compare_sql(pattern, &ComparisonOp::Like);
                return Expr::Literal(truth_value(if negated { truth.negate() } else { truth }));
            }
            Expr::Like {
                expr: Box::new(expr),
                pattern: Box::new(pattern),
                negated,
            }
        }
        Expr::InList {
            expr,
            list,
            negated,
        } => Expr::InList {
            expr: Box::new(fold_expr(*expr)),
            list: list.into_iter().map(fold_expr).collect(),
            negated,
        },
        Expr::InSubquery {
            expr,
            query,
            negated,
        } => Expr::InSubquery {
            expr: Box::new(fold_expr(*expr)),
            query: Box::new(Optimizer::optimize_query(*query)),
            negated,
        },
        Expr::Exists { query, negated } => Expr::Exists {
            query: Box::new(Optimizer::optimize_query(*query)),
            negated,
        },
        Expr::ScalarSubquery(query) => {
            Expr::ScalarSubquery(Box::new(Optimizer::optimize_query(*query)))
        }
        Expr::Aggregate {
            function,
            expr,
            distinct,
        } => Expr::Aggregate {
            function,
            expr: expr.map(|expr| Box::new(fold_expr(*expr))),
            distinct,
        },
        other => other,
    }
}

fn fold_binary(left: &Value, operator: &BinaryOp, right: &Value) -> Option<Value> {
    if left.is_null() || right.is_null() {
        return Some(Value::Null);
    }
    match operator {
        BinaryOp::Compare(operator) => Some(truth_value(left.compare_sql(right, operator))),
        BinaryOp::And => Some(truth_value(left.truth().and(right.truth()))),
        BinaryOp::Or => Some(truth_value(left.truth().or(right.truth()))),
        BinaryOp::Add => fold_numeric(left, right, |a, b| a + b, |a, b| a + b),
        BinaryOp::Subtract => fold_numeric(left, right, |a, b| a - b, |a, b| a - b),
        BinaryOp::Multiply => fold_numeric(left, right, |a, b| a * b, |a, b| a * b),
        BinaryOp::Divide => {
            if matches!(right, Value::Integer(0) | Value::Float(0.0)) {
                None
            } else {
                fold_numeric(left, right, |a, b| a / b, |a, b| a / b)
            }
        }
    }
}

fn fold_numeric(
    left: &Value,
    right: &Value,
    integer: impl FnOnce(i64, i64) -> i64,
    float: impl FnOnce(f64, f64) -> f64,
) -> Option<Value> {
    match (left, right) {
        (Value::Integer(left), Value::Integer(right)) => {
            Some(Value::Integer(integer(*left, *right)))
        }
        (Value::Integer(left), Value::Float(right)) => {
            Some(Value::Float(float(*left as f64, *right)))
        }
        (Value::Float(left), Value::Integer(right)) => {
            Some(Value::Float(float(*left, *right as f64)))
        }
        (Value::Float(left), Value::Float(right)) => Some(Value::Float(float(*left, *right))),
        _ => None,
    }
}

fn truth_value(truth: SqlTruth) -> Value {
    match truth {
        SqlTruth::True => Value::Boolean(true),
        SqlTruth::False => Value::Boolean(false),
        SqlTruth::Unknown => Value::Null,
    }
}

impl<'a> Planner<'a> {
    pub fn new(catalog: &'a Catalog) -> Self {
        Self { catalog }
    }

    pub fn plan(&self, query: &Query) -> LogicalPlan {
        let mut plan = query
            .from
            .as_ref()
            .map(|source| self.plan_source(source, query.selection.as_ref()))
            .unwrap_or(LogicalPlan::Values);

        for join in &query.joins {
            let right = self.plan_source(&join.source, None);
            plan = LogicalPlan::Join {
                join_type: join.join_type.clone(),
                algorithm: if join.on.as_ref().is_some_and(is_hash_join) {
                    JoinAlgorithm::Hash
                } else {
                    JoinAlgorithm::NestedLoop
                },
                on: join.on.clone(),
                left: Box::new(plan),
                right: Box::new(right),
            };
        }

        if let Some(predicate) = &query.selection {
            plan = LogicalPlan::Filter {
                predicate: predicate.clone(),
                input: Box::new(plan),
            };
        }
        if !query.group_by.is_empty()
            || query.projection.iter().any(select_item_has_aggregate)
            || query.having.as_ref().is_some_and(expr_has_aggregate)
        {
            plan = LogicalPlan::Aggregate {
                group_by: query.group_by.clone(),
                having: query.having.clone(),
                input: Box::new(plan),
            };
        }
        plan = LogicalPlan::Projection {
            expressions: query.projection.clone(),
            input: Box::new(plan),
        };
        if !query.order_by.is_empty() {
            plan = if let Some(limit) = query.limit {
                LogicalPlan::TopN {
                    order_by: query.order_by.clone(),
                    limit,
                    input: Box::new(plan),
                }
            } else {
                LogicalPlan::Sort {
                    order_by: query.order_by.clone(),
                    input: Box::new(plan),
                }
            };
        } else if let Some(limit) = query.limit {
            plan = LogicalPlan::Limit {
                limit,
                input: Box::new(plan),
            };
        }
        for cte in query.ctes.iter().rev() {
            plan = LogicalPlan::MaterializeCte {
                name: cte.name.clone(),
                cte: Box::new(self.plan(&cte.query)),
                input: Box::new(plan),
            };
        }
        plan
    }

    fn plan_source(&self, source: &TableSource, selection: Option<&Expr>) -> LogicalPlan {
        match source {
            TableSource::Table { name, alias } => LogicalPlan::TableScan {
                table: name.clone(),
                alias: alias.clone(),
                index: self
                    .catalog
                    .tables
                    .get(name)
                    .and_then(|table| table.best_index_name(selection)),
            },
            TableSource::Derived { query, alias } => LogicalPlan::DerivedScan {
                alias: alias.clone(),
                input: Box::new(self.plan(query)),
            },
        }
    }
}

impl LogicalPlan {
    pub fn explain_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        self.write_explain(0, &mut lines);
        lines
    }

    fn write_explain(&self, depth: usize, lines: &mut Vec<String>) {
        let indent = "  ".repeat(depth);
        match self {
            LogicalPlan::Values => lines.push(format!("{indent}Values(one_row)")),
            LogicalPlan::TableScan {
                table,
                alias,
                index,
            } => lines.push(match index {
                Some(index) => format!(
                    "{indent}IndexScan(table={table}, alias={}, index={index}, bloom=true)",
                    alias.as_deref().unwrap_or(table)
                ),
                None => format!(
                    "{indent}SeqScan(table={table}, alias={})",
                    alias.as_deref().unwrap_or(table)
                ),
            }),
            LogicalPlan::DerivedScan { alias, input } => {
                lines.push(format!("{indent}DerivedScan(alias={alias})"));
                input.write_explain(depth + 1, lines);
            }
            LogicalPlan::Filter { input, .. } => {
                lines.push(format!(
                    "{indent}Filter(predicate_pushdown=true, constants_folded=true)"
                ));
                input.write_explain(depth + 1, lines);
            }
            LogicalPlan::Projection { input, .. } => {
                lines.push(format!("{indent}Projection(pruned=true)"));
                input.write_explain(depth + 1, lines);
            }
            LogicalPlan::Join {
                join_type,
                algorithm,
                left,
                right,
                ..
            } => {
                lines.push(format!("{indent}{algorithm:?}Join(type={join_type:?})"));
                left.write_explain(depth + 1, lines);
                right.write_explain(depth + 1, lines);
            }
            LogicalPlan::Aggregate { input, .. } => {
                lines.push(format!("{indent}HashAggregate"));
                input.write_explain(depth + 1, lines);
            }
            LogicalPlan::Sort { input, .. } => {
                lines.push(format!("{indent}Sort"));
                input.write_explain(depth + 1, lines);
            }
            LogicalPlan::TopN { limit, input, .. } => {
                lines.push(format!("{indent}TopN(limit={limit})"));
                input.write_explain(depth + 1, lines);
            }
            LogicalPlan::Limit { limit, input } => {
                lines.push(format!("{indent}Limit({limit})"));
                input.write_explain(depth + 1, lines);
            }
            LogicalPlan::MaterializeCte { name, cte, input } => {
                lines.push(format!("{indent}MaterializeCTE(name={name})"));
                cte.write_explain(depth + 1, lines);
                input.write_explain(depth + 1, lines);
            }
        }
    }
}

fn is_hash_join(expression: &Expr) -> bool {
    matches!(
        expression,
        Expr::Binary {
            left,
            op: BinaryOp::Compare(ComparisonOp::Equal),
            right,
        } if matches!(&**left, Expr::Column { .. })
            && matches!(&**right, Expr::Column { .. })
    )
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
