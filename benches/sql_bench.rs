use criterion::{black_box, criterion_group, criterion_main, Criterion, BenchmarkId};
use rustydb::sql::{SQLDatabase, ExecutionResult};

fn check_result(result: ExecutionResult) {
    match result {
        ExecutionResult::Error(e) => panic!("SQL execution failed: {}", e),
        _ => {}
    }
}

fn benchmark_create_table(c: &mut Criterion) {
    c.bench_function("sql_create_table", |b| {
        b.iter(|| {
            let db = SQLDatabase::new();
            
            check_result(db.execute(black_box(
                "CREATE TABLE test (id INTEGER PRIMARY KEY, name TEXT, value FLOAT)"
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
        check_result(db.execute(&format!("INSERT INTO test VALUES ({}, 'name_{}', {}.99)", i, i, i)));
    }

    c.bench_function("sql_select_all", |b| {
        b.iter(|| {
            let _ = db.execute(black_box("SELECT * FROM test"));
        });
    });
}

fn benchmark_select_with_where(c: &mut Criterion) {
    let db = SQLDatabase::new();
    
    check_result(db.execute("CREATE TABLE test (id INTEGER PRIMARY KEY, name TEXT, category TEXT, value FLOAT)"));
    
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
            let _ = db.execute(black_box("SELECT * FROM test WHERE category = 'Electronics'"));
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
                "SELECT * FROM test WHERE category = 'Electronics' AND value > 500"
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
            let _ = db.execute(black_box("SELECT * FROM test WHERE email LIKE '%@gmail.com'"));
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
        check_result(db.execute(&format!("INSERT INTO test VALUES ({}, 'item_{}', {}.99)", i, i, i)));
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
    
    check_result(db.execute("CREATE TABLE test (id INTEGER PRIMARY KEY, name TEXT, value FLOAT, active BOOLEAN)"));
    
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
            let query = format!("UPDATE test SET value = {} WHERE id = {}", counter as f64, id);
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
    
    check_result(db.execute("CREATE TABLE products (id INTEGER PRIMARY KEY, name TEXT, price FLOAT, stock INTEGER)"));
    
    for i in 0..500 {
        check_result(db.execute(&format!(
            "INSERT INTO products VALUES ({}, 'product_{}', {}.99, {})",
            i, i, i, i * 10
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
                    counter, counter % 500
                );
                let _ = db.execute(black_box(&query));
            } else if op < 19 {
                // INSERT (15%)
                let query = format!(
                    "INSERT INTO products VALUES ({}, 'new_product_{}', {}.99, {})",
                    counter, counter, counter % 1000, counter * 5
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
    
    for size in [100, 1000, 10000].iter() {
        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, &size| {
            let db = SQLDatabase::new();
            
            check_result(db.execute("CREATE TABLE test (id INTEGER PRIMARY KEY, name TEXT, value FLOAT)"));
            
            // Populate table
            for i in 0..size {
                check_result(db.execute(&format!(
                    "INSERT INTO test VALUES ({}, 'item_{}', {}.99)",
                    i, i, i
                )));
            }
            
            b.iter(|| {
                let _ = db.execute(black_box("SELECT * FROM test WHERE value > 50"));
            });
        });
    }
    
    group.finish();
}

fn benchmark_complex_queries(c: &mut Criterion) {
    let db = SQLDatabase::new();
    
    // Create a more complex schema
    check_result(db.execute("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, age INTEGER, active BOOLEAN)"));
    check_result(db.execute("CREATE TABLE orders (id INTEGER PRIMARY KEY, user_id INTEGER, amount FLOAT, status TEXT)"));
    
    // Populate users
    for i in 0..500 {
        let active = if i % 3 == 0 { "FALSE" } else { "TRUE" };
        check_result(db.execute(&format!(
            "INSERT INTO users VALUES ({}, 'user_{}', {}, {})",
            i, i, 20 + (i % 50), active
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
            i, i % 500, (i * 10) % 1000, status
        )));
    }
    
    c.bench_function("sql_complex_where_and_or", |b| {
        b.iter(|| {
            let _ = db.execute(black_box(
                "SELECT * FROM orders WHERE status = 'completed' AND amount > 500"
            ));
        });
    });
    
    c.bench_function("sql_complex_multi_condition", |b| {
        b.iter(|| {
            let _ = db.execute(black_box(
                "SELECT * FROM users WHERE age > 30 AND active = TRUE"
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
);

criterion_main!(benches);
