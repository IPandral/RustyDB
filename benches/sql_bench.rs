use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use rustydb::sql::{ExecutionResult, SQLDatabase};

fn check_result(result: ExecutionResult) {
    if let ExecutionResult::Error(error) = result {
        panic!("SQL execution failed: {error}");
    }
}

fn check_rows(result: ExecutionResult, expected: usize) {
    match result {
        ExecutionResult::Select(result) => assert_eq!(result.rows.len(), expected),
        ExecutionResult::Error(error) => panic!("SQL execution failed: {error}"),
        other => panic!("Expected SELECT result, got {other:?}"),
    }
}

fn benchmark_create_table(c: &mut Criterion) {
    c.bench_function("sql_create_table", |b| {
        b.iter(|| {
            let db = SQLDatabase::new();

            check_result(db.execute(black_box(
                "CREATE TABLE test (id INTEGER PRIMARY KEY, name TEXT, value FLOAT)",
            )));
        });
    });
}

fn benchmark_insert(c: &mut Criterion) {
    let db = SQLDatabase::new();

    check_result(db.execute("CREATE TABLE test (id INTEGER PRIMARY KEY, name TEXT, value FLOAT)"));

    c.bench_function("sql_insert", |b| {
        let mut counter = 0;
        b.iter(|| {
            let query = format!(
                "INSERT INTO test VALUES ({}, 'name_{}', {}.99)",
                counter, counter, counter
            );
            let _ = db.execute(black_box(&query));
            counter += 1;
        });
    });
}

fn benchmark_select_all(c: &mut Criterion) {
    let db = SQLDatabase::new();

    check_result(db.execute("CREATE TABLE test (id INTEGER PRIMARY KEY, name TEXT, value FLOAT)"));

    for i in 0..1000 {
        check_result(db.execute(&format!(
            "INSERT INTO test VALUES ({}, 'name_{}', {}.99)",
            i, i, i
        )));
    }

    c.bench_function("sql_select_all", |b| {
        b.iter(|| {
            let _ = db.execute(black_box("SELECT * FROM test"));
        });
    });
}

fn benchmark_select_with_where(c: &mut Criterion) {
    let db = SQLDatabase::new();

    check_result(db.execute(
        "CREATE TABLE test (id INTEGER PRIMARY KEY, name TEXT, category TEXT, value FLOAT)",
    ));

    let categories = ["Electronics", "Furniture", "Office", "Accessories"];
    for i in 0..1000 {
        let category = categories[i % 4];
        check_result(db.execute(&format!(
            "INSERT INTO test VALUES ({}, 'item_{}', '{}', {}.99)",
            i, i, category, i
        )));
    }

    c.bench_function("sql_select_where_equality", |b| {
        b.iter(|| {
            let _ = db.execute(black_box(
                "SELECT * FROM test WHERE category = 'Electronics'",
            ));
        });
    });

    c.bench_function("sql_select_where_comparison", |b| {
        b.iter(|| {
            let _ = db.execute(black_box("SELECT * FROM test WHERE value > 500"));
        });
    });

    c.bench_function("sql_select_where_and", |b| {
        b.iter(|| {
            let _ = db.execute(black_box(
                "SELECT * FROM test WHERE category = 'Electronics' AND value > 500",
            ));
        });
    });
}

fn benchmark_select_with_like(c: &mut Criterion) {
    let db = SQLDatabase::new();

    check_result(db.execute("CREATE TABLE test (id INTEGER PRIMARY KEY, name TEXT, email TEXT)"));

    let domains = ["gmail.com", "yahoo.com", "example.com", "test.org"];
    for i in 0..1000 {
        let domain = domains[i % 4];
        check_result(db.execute(&format!(
            "INSERT INTO test VALUES ({}, 'user_{}', 'user{}@{}')",
            i, i, i, domain
        )));
    }

    c.bench_function("sql_select_like_suffix", |b| {
        b.iter(|| {
            let _ = db.execute(black_box(
                "SELECT * FROM test WHERE email LIKE '%@gmail.com'",
            ));
        });
    });

    c.bench_function("sql_select_like_prefix", |b| {
        b.iter(|| {
            let _ = db.execute(black_box("SELECT * FROM test WHERE name LIKE 'user_1%'"));
        });
    });
}

fn benchmark_select_with_order_and_limit(c: &mut Criterion) {
    let db = SQLDatabase::new();

    check_result(db.execute("CREATE TABLE test (id INTEGER PRIMARY KEY, name TEXT, value FLOAT)"));

    for i in 0..1000 {
        check_result(db.execute(&format!(
            "INSERT INTO test VALUES ({}, 'item_{}', {}.99)",
            i, i, i
        )));
    }

    c.bench_function("sql_select_order_by", |b| {
        b.iter(|| {
            let _ = db.execute(black_box("SELECT * FROM test ORDER BY value DESC"));
        });
    });

    c.bench_function("sql_select_order_by_limit", |b| {
        b.iter(|| {
            let _ = db.execute(black_box("SELECT * FROM test ORDER BY value DESC LIMIT 10"));
        });
    });
}

fn benchmark_update(c: &mut Criterion) {
    let db = SQLDatabase::new();

    check_result(db.execute(
        "CREATE TABLE test (id INTEGER PRIMARY KEY, name TEXT, value FLOAT, active BOOLEAN)",
    ));

    for i in 0..1000 {
        check_result(db.execute(&format!(
            "INSERT INTO test VALUES ({}, 'item_{}', {}.99, TRUE)",
            i, i, i
        )));
    }

    c.bench_function("sql_update_single_row", |b| {
        let mut counter = 0;
        b.iter(|| {
            let id = counter % 1000;
            let query = format!(
                "UPDATE test SET value = {} WHERE id = {}",
                counter as f64, id
            );
            let _ = db.execute(black_box(&query));
            counter += 1;
        });
    });

    c.bench_function("sql_update_multiple_rows", |b| {
        let mut counter = 0;
        b.iter(|| {
            let threshold = (counter % 500) as f64;
            let query = format!("UPDATE test SET active = FALSE WHERE value < {}", threshold);
            let _ = db.execute(black_box(&query));
            counter += 1;
        });
    });
}

fn benchmark_delete(c: &mut Criterion) {
    c.bench_function("sql_delete_single_row", |b| {
        let db = SQLDatabase::new();

        check_result(db.execute("CREATE TABLE test (id INTEGER PRIMARY KEY, name TEXT)"));

        for i in 0..10000 {
            check_result(db.execute(&format!("INSERT INTO test VALUES ({}, 'item_{}')", i, i)));
        }

        let mut counter = 0;
        b.iter(|| {
            let query = format!("DELETE FROM test WHERE id = {}", counter);
            let _ = db.execute(black_box(&query));
            counter += 1;
        });
    });
}

fn benchmark_mixed_sql_workload(c: &mut Criterion) {
    let db = SQLDatabase::new();

    check_result(db.execute(
        "CREATE TABLE products (id INTEGER PRIMARY KEY, name TEXT, price FLOAT, stock INTEGER)",
    ));

    for i in 0..500 {
        check_result(db.execute(&format!(
            "INSERT INTO products VALUES ({}, 'product_{}', {}.99, {})",
            i,
            i,
            i,
            i * 10
        )));
    }

    c.bench_function("sql_mixed_workload", |b| {
        let mut counter = 500;
        b.iter(|| {
            // Simulate realistic mixed workload
            // 60% SELECT, 20% UPDATE, 15% INSERT, 5% DELETE
            let op = counter % 20;

            if op < 12 {
                // SELECT (60%)
                let query = if op < 4 {
                    "SELECT * FROM products WHERE price > 100".to_string()
                } else if op < 8 {
                    format!("SELECT * FROM products WHERE id = {}", counter % 500)
                } else {
                    "SELECT * FROM products ORDER BY price DESC LIMIT 10".to_string()
                };
                let _ = db.execute(black_box(&query));
            } else if op < 16 {
                // UPDATE (20%)
                let query = format!(
                    "UPDATE products SET stock = {} WHERE id = {}",
                    counter,
                    counter % 500
                );
                let _ = db.execute(black_box(&query));
            } else if op < 19 {
                // INSERT (15%)
                let query = format!(
                    "INSERT INTO products VALUES ({}, 'new_product_{}', {}.99, {})",
                    counter,
                    counter,
                    counter % 1000,
                    counter * 5
                );
                let _ = db.execute(black_box(&query)); // May fail on duplicate, that's ok
            } else {
                // DELETE (5%)
                let query = format!("DELETE FROM products WHERE id = {}", counter);
                let _ = db.execute(black_box(&query)); // May not find row, that's ok
            }

            counter += 1;
        });
    });
}

fn benchmark_table_sizes(c: &mut Criterion) {
    let mut group = c.benchmark_group("sql_table_sizes");

    for size in [100, 1000, 10000, 100000].iter() {
        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, &size| {
            let db = SQLDatabase::new();

            check_result(
                db.execute("CREATE TABLE test (id INTEGER PRIMARY KEY, name TEXT, value FLOAT)"),
            );

            // Populate table
            let values = (0..size)
                .map(|i| format!("({}, 'item_{}', {}.99)", i, i, i))
                .collect::<Vec<_>>()
                .join(",");
            check_result(db.execute(&format!("INSERT INTO test VALUES {values}")));

            b.iter(|| {
                let _ = db.execute(black_box("SELECT * FROM test WHERE value > 50"));
            });
        });
    }

    group.finish();
}

fn benchmark_indexes(c: &mut Criterion) {
    let mut group = c.benchmark_group("sql_index_lookup");
    group.sample_size(30);
    for size in [1000, 10000, 100000] {
        let sequential = SQLDatabase::new();
        let indexed = SQLDatabase::new();
        check_result(
            sequential.execute("CREATE TABLE events (id INT, category TEXT, sequence INT)"),
        );
        check_result(indexed.execute("CREATE TABLE events (id INT, category TEXT, sequence INT)"));
        let values = (0..size)
            .map(|i| format!("({}, 'category_{}', {})", i, i % 20, i))
            .collect::<Vec<_>>()
            .join(",");
        check_result(sequential.execute(&format!("INSERT INTO events VALUES {values}")));
        check_result(indexed.execute(&format!("INSERT INTO events VALUES {values}")));
        check_result(
            indexed.execute("CREATE INDEX events_category_sequence ON events(category, sequence)"),
        );
        check_rows(
            sequential
                .execute("SELECT * FROM events WHERE category = 'category_7' AND sequence >= 500"),
            (500..size).filter(|value| value % 20 == 7).count(),
        );
        check_rows(
            indexed
                .execute("SELECT * FROM events WHERE category = 'category_7' AND sequence >= 500"),
            (500..size).filter(|value| value % 20 == 7).count(),
        );
        group.bench_with_input(BenchmarkId::new("sequential_range", size), &size, |b, _| {
            b.iter(|| {
                let _ = sequential.execute(black_box(
                    "SELECT * FROM events WHERE category = 'category_7' AND sequence >= 500",
                ));
            });
        });
        group.bench_with_input(BenchmarkId::new("indexed_range", size), &size, |b, _| {
            b.iter(|| {
                let _ = indexed.execute(black_box(
                    "SELECT * FROM events WHERE category = 'category_7' AND sequence >= 500",
                ));
            });
        });
    }
    group.finish();
}

fn benchmark_advanced_sql(c: &mut Criterion) {
    let db = SQLDatabase::new();
    check_result(db.execute("CREATE TABLE users (id INT PRIMARY KEY, team TEXT, active BOOLEAN)"));
    check_result(db.execute(
        "CREATE TABLE orders (id INT PRIMARY KEY, user_id INT, amount FLOAT, status TEXT)",
    ));
    let users = (0..1000)
        .map(|i| format!("({}, 'team_{}', {})", i, i % 10, i % 3 != 0))
        .collect::<Vec<_>>()
        .join(",");
    check_result(db.execute(&format!("INSERT INTO users VALUES {users}")));
    let orders = (0..10000)
        .map(|i| {
            format!(
                "({}, {}, {}.99, '{}')",
                i,
                i % 1000,
                i % 500,
                if i % 2 == 0 { "paid" } else { "pending" }
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    check_result(db.execute(&format!("INSERT INTO orders VALUES {orders}")));
    check_result(db.execute("CREATE INDEX orders_user_status ON orders(user_id, status)"));

    check_rows(
        db.execute(
            "SELECT users.team, COUNT(*) AS count, SUM(orders.amount) AS total
             FROM users INNER JOIN orders ON users.id = orders.user_id
             WHERE orders.status = 'paid'
             GROUP BY users.team HAVING COUNT(*) > 0",
        ),
        5,
    );

    c.bench_function("sql_hash_join_aggregate", |b| {
        b.iter(|| {
            let _ = db.execute(black_box(
                "SELECT users.team, COUNT(*) AS count, SUM(orders.amount) AS total
                 FROM users INNER JOIN orders ON users.id = orders.user_id
                 WHERE orders.status = 'paid'
                 GROUP BY users.team HAVING COUNT(*) > 0",
            ));
        });
    });

    c.bench_function("sql_cte_subquery", |b| {
        b.iter(|| {
            let _ = db.execute(black_box(
                "WITH paid AS (
                    SELECT user_id, SUM(amount) AS total
                    FROM orders WHERE status = 'paid' GROUP BY user_id
                 )
                 SELECT users.id, paid.total
                 FROM users LEFT JOIN paid ON users.id = paid.user_id
                 WHERE users.id IN (SELECT user_id FROM orders WHERE amount > 250)
                 ORDER BY paid.total DESC LIMIT 25",
            ));
        });
    });
}

fn benchmark_transactions_constraints_and_cache(c: &mut Criterion) {
    c.bench_function("sql_transaction_10_writes", |b| {
        let db = SQLDatabase::new();
        check_result(db.execute("CREATE TABLE tx (id INT PRIMARY KEY, value TEXT)"));
        let session = db.session();
        let mut counter = 0u64;
        b.iter(|| {
            check_result(session.begin());
            for offset in 0..10 {
                check_result(session.execute(&format!(
                    "INSERT INTO tx VALUES ({}, 'value')",
                    counter + offset
                )));
            }
            check_result(session.commit());
            counter += 10;
        });
    });

    c.bench_function("sql_constraint_insert", |b| {
        let db = SQLDatabase::new();
        check_result(db.execute("CREATE TABLE parents (id INT PRIMARY KEY)"));
        check_result(db.execute(
            "CREATE TABLE children (
                id INT PRIMARY KEY,
                parent_id INT,
                score INT CHECK (score >= 0),
                UNIQUE (parent_id, score),
                FOREIGN KEY (parent_id) REFERENCES parents(id)
            )",
        ));
        check_result(db.execute("INSERT INTO parents VALUES (1)"));
        let mut counter = 0u64;
        b.iter(|| {
            check_result(db.execute(&format!(
                "INSERT INTO children VALUES ({}, 1, {})",
                counter, counter
            )));
            counter += 1;
        });
    });

    let warm = SQLDatabase::new();
    check_result(warm.execute("CREATE TABLE cache_test (id INT PRIMARY KEY, value TEXT)"));
    for i in 0..1000 {
        check_result(warm.execute(&format!("INSERT INTO cache_test VALUES ({i}, 'value')")));
    }
    check_rows(warm.execute("SELECT * FROM cache_test WHERE id = 500"), 1);
    c.bench_function("sql_plan_cache_warm", |b| {
        b.iter(|| {
            let _ = warm.execute(black_box("SELECT * FROM cache_test WHERE id = 500"));
        });
    });
    c.bench_function("sql_plan_cache_cold", |b| {
        let mut counter = 0;
        b.iter(|| {
            let query = format!(
                "SELECT * FROM cache_test WHERE id = 500 AND {} = {}",
                counter, counter
            );
            let _ = warm.execute(black_box(&query));
            counter += 1;
        });
    });
}

fn benchmark_complex_queries(c: &mut Criterion) {
    let db = SQLDatabase::new();

    // Create a more complex schema
    check_result(db.execute(
        "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, age INTEGER, active BOOLEAN)",
    ));
    check_result(db.execute(
        "CREATE TABLE orders (id INTEGER PRIMARY KEY, user_id INTEGER, amount FLOAT, status TEXT)",
    ));

    // Populate users
    for i in 0..500 {
        let active = if i % 3 == 0 { "FALSE" } else { "TRUE" };
        check_result(db.execute(&format!(
            "INSERT INTO users VALUES ({}, 'user_{}', {}, {})",
            i,
            i,
            20 + (i % 50),
            active
        )));
    }

    // Populate orders
    for i in 0..2000 {
        let status = match i % 4 {
            0 => "pending",
            1 => "completed",
            2 => "shipped",
            _ => "cancelled",
        };
        check_result(db.execute(&format!(
            "INSERT INTO orders VALUES ({}, {}, {}.99, '{}')",
            i,
            i % 500,
            (i * 10) % 1000,
            status
        )));
    }

    c.bench_function("sql_complex_where_and_or", |b| {
        b.iter(|| {
            let _ = db.execute(black_box(
                "SELECT * FROM orders WHERE status = 'completed' AND amount > 500",
            ));
        });
    });

    c.bench_function("sql_complex_multi_condition", |b| {
        b.iter(|| {
            let _ = db.execute(black_box(
                "SELECT * FROM users WHERE age > 30 AND active = TRUE",
            ));
        });
    });
}

criterion_group!(
    benches,
    benchmark_create_table,
    benchmark_insert,
    benchmark_select_all,
    benchmark_select_with_where,
    benchmark_select_with_like,
    benchmark_select_with_order_and_limit,
    benchmark_update,
    benchmark_delete,
    benchmark_mixed_sql_workload,
    benchmark_table_sizes,
    benchmark_complex_queries,
    benchmark_indexes,
    benchmark_advanced_sql,
    benchmark_transactions_constraints_and_cache,
);

criterion_main!(benches);
