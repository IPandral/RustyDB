#!/usr/bin/env python3
"""
RustyDB vs MySQL Benchmark Comparison

Runs identical SQL operations against RustyDB (via MySQL wire protocol) and a
real MySQL 8.0 instance, then prints a side-by-side comparison table.

Benchmarks mirror those in benches/sql_bench.rs so results are directly
comparable with the native Criterion benchmarks.

Usage:
    # Start MySQL:
    docker compose -f benches/compare/docker-compose.mysql.yml up -d

    # Start RustyDB (separate terminal):
    cargo run --release --features server -- --server --memory

    # Run benchmarks:
    python benches/compare/bench_compare.py

    # Run subset:
    python benches/compare/bench_compare.py --filter select

    # Export CSV:
    python benches/compare/bench_compare.py --csv results.csv
"""

import argparse
import csv
import math
import os
import re
import statistics
import sys
import time

try:
    import mysql.connector
    from mysql.connector import Error as MySQLError
except ImportError:
    print("ERROR: mysql-connector-python is not installed.")
    print("Install it with:  pip install mysql-connector-python")
    sys.exit(1)


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------

def parse_args():
    p = argparse.ArgumentParser(description="RustyDB vs MySQL benchmark comparison")
    p.add_argument("--rustydb-host", default=os.environ.get("RUSTYDB_TEST_HOST", "127.0.0.1"))
    p.add_argument("--rustydb-port", type=int, default=int(os.environ.get("RUSTYDB_WIRE_PORT", "3307")))
    p.add_argument("--rustydb-user", default=os.environ.get("RUSTYDB_USERNAME", "root"))
    p.add_argument("--rustydb-pass", default=os.environ.get("RUSTYDB_PASSWORD", ""))
    p.add_argument("--mysql-host", default="127.0.0.1")
    p.add_argument("--mysql-port", type=int, default=3306)
    p.add_argument("--mysql-user", default="root")
    p.add_argument("--mysql-pass", default="bench")
    p.add_argument("--mysql-db", default="bench")
    p.add_argument("--iterations", "-n", type=int, default=100)
    p.add_argument("--warmup", "-w", type=int, default=10)
    p.add_argument("--filter", default=None, help="Regex to filter benchmark names")
    p.add_argument("--csv", default=None, metavar="FILE", help="Write results to CSV")
    return p.parse_args()


# ---------------------------------------------------------------------------
# Connection helpers
# ---------------------------------------------------------------------------

def try_connect(host, port, user, password, database=None, timeout=30):
    """Attempt connection with retries and exponential backoff."""
    deadline = time.time() + timeout
    delay = 0.5
    last_err = None
    while time.time() < deadline:
        try:
            kwargs = dict(
                host=host,
                port=port,
                user=user,
                password=password,
                connection_timeout=5,
                auth_plugin="mysql_native_password",
                use_pure=True,
            )
            if database:
                kwargs["database"] = database
            conn = mysql.connector.connect(**kwargs)
            return conn
        except Exception as e:
            last_err = e
            time.sleep(delay)
            delay = min(delay * 2, 4)
    return None


def safe_drop(conn, table):
    try:
        c = conn.cursor()
        c.execute(f"DROP TABLE {table}")
        conn.commit()
        c.close()
    except Exception:
        pass


def execute(conn, sql):
    c = conn.cursor()
    c.execute(sql)
    if c.description is not None:
        rows = c.fetchall()
        c.close()
        return rows
    conn.commit()
    affected = c.rowcount
    c.close()
    return affected


def batch_insert(conn, table, rows_sql, batch_size=100):
    """Insert rows in batches. rows_sql is a list of value-tuple strings like
    ``(1, 'name_1', 1.99)``."""
    for i in range(0, len(rows_sql), batch_size):
        chunk = rows_sql[i:i + batch_size]
        sql = f"INSERT INTO {table} VALUES {', '.join(chunk)}"
        execute(conn, sql)


# ---------------------------------------------------------------------------
# Benchmark result
# ---------------------------------------------------------------------------

class BenchResult:
    def __init__(self, name, times_ms):
        self.name = name
        self.times = sorted(times_ms)

    @property
    def mean(self):
        return statistics.mean(self.times)

    @property
    def median(self):
        return statistics.median(self.times)

    @property
    def stdev(self):
        return statistics.stdev(self.times) if len(self.times) > 1 else 0.0

    @property
    def min(self):
        return self.times[0]

    @property
    def max(self):
        return self.times[-1]


# ---------------------------------------------------------------------------
# Timing harness
# ---------------------------------------------------------------------------

def run_bench(conn, name, setup_fn, bench_fn, teardown_fn, warmup, iterations):
    """Run a single benchmark returning a BenchResult."""
    setup_fn(conn)

    state = {"counter": 0}

    for _ in range(warmup):
        bench_fn(conn, state)
        state["counter"] += 1

    times = []
    for _ in range(iterations):
        t0 = time.perf_counter()
        bench_fn(conn, state)
        t1 = time.perf_counter()
        times.append((t1 - t0) * 1000)  # ms
        state["counter"] += 1

    teardown_fn(conn)

    return BenchResult(name, times)


# ---------------------------------------------------------------------------
# Data generators (match benches/sql_bench.rs exactly)
# ---------------------------------------------------------------------------

CATEGORIES = ["Electronics", "Furniture", "Office", "Accessories"]
DOMAINS = ["gmail.com", "yahoo.com", "example.com", "test.org"]
STATUSES = ["pending", "completed", "shipped", "cancelled"]


def gen_basic_rows(n):
    return [f"({i}, 'name_{i}', {i}.99)" for i in range(n)]


def gen_category_rows(n):
    return [
        f"({i}, 'item_{i}', '{CATEGORIES[i % 4]}', {i}.99)"
        for i in range(n)
    ]


def gen_email_rows(n):
    return [
        f"({i}, 'user_{i}', 'user{i}@{DOMAINS[i % 4]}')"
        for i in range(n)
    ]


def gen_update_rows(n):
    return [
        f"({i}, 'item_{i}', {i}.99, TRUE)"
        for i in range(n)
    ]


def gen_delete_rows(n):
    return [f"({i}, 'item_{i}')" for i in range(n)]


def gen_product_rows(n):
    return [
        f"({i}, 'product_{i}', {i}.99, {i * 10})"
        for i in range(n)
    ]


def gen_user_rows(n):
    return [
        f"({i}, 'user_{i}', {20 + (i % 50)}, {'FALSE' if i % 3 == 0 else 'TRUE'})"
        for i in range(n)
    ]


def gen_order_rows(n):
    return [
        f"({i}, {i % 500}, {(i * 10) % 1000}.99, '{STATUSES[i % 4]}')"
        for i in range(n)
    ]


# ---------------------------------------------------------------------------
# Benchmark definitions
# ---------------------------------------------------------------------------

def build_benchmarks():
    benchmarks = []

    # 1. create_table
    def ct_setup(conn):
        safe_drop(conn, "test")

    def ct_bench(conn, state):
        safe_drop(conn, "test")
        execute(conn, "CREATE TABLE test (id INTEGER PRIMARY KEY, name TEXT, value FLOAT)")

    def ct_teardown(conn):
        safe_drop(conn, "test")

    benchmarks.append(("create_table", ct_setup, ct_bench, ct_teardown))

    # 2. insert
    def ins_setup(conn):
        safe_drop(conn, "test")
        execute(conn, "CREATE TABLE test (id INTEGER PRIMARY KEY, name TEXT, value FLOAT)")

    counter_offset = [0]

    def ins_bench(conn, state):
        i = 100_000 + state["counter"]
        execute(conn, f"INSERT INTO test VALUES ({i}, 'name_{i}', {i}.99)")

    def ins_teardown(conn):
        safe_drop(conn, "test")

    benchmarks.append(("insert", ins_setup, ins_bench, ins_teardown))

    # 3. select_all (1K rows)
    def sa_setup(conn):
        safe_drop(conn, "test")
        execute(conn, "CREATE TABLE test (id INTEGER PRIMARY KEY, name TEXT, value FLOAT)")
        batch_insert(conn, "test", gen_basic_rows(1000))

    def sa_bench(conn, state):
        execute(conn, "SELECT * FROM test")

    def sa_teardown(conn):
        safe_drop(conn, "test")

    benchmarks.append(("select_all", sa_setup, sa_bench, sa_teardown))

    # 4-6. select_where (equality / comparison / AND)
    def sw_setup(conn):
        safe_drop(conn, "test")
        execute(conn, "CREATE TABLE test (id INTEGER PRIMARY KEY, name TEXT, category TEXT, value FLOAT)")
        batch_insert(conn, "test", gen_category_rows(1000))

    def sw_teardown(conn):
        safe_drop(conn, "test")

    def sw_eq(conn, state):
        execute(conn, "SELECT * FROM test WHERE category = 'Electronics'")

    def sw_cmp(conn, state):
        execute(conn, "SELECT * FROM test WHERE value > 500")

    def sw_and(conn, state):
        execute(conn, "SELECT * FROM test WHERE category = 'Electronics' AND value > 500")

    benchmarks.append(("select_where_eq", sw_setup, sw_eq, sw_teardown))
    benchmarks.append(("select_where_cmp", sw_setup, sw_cmp, sw_teardown))
    benchmarks.append(("select_where_and", sw_setup, sw_and, sw_teardown))

    # 7-8. select_like (suffix / prefix)
    def sl_setup(conn):
        safe_drop(conn, "test")
        execute(conn, "CREATE TABLE test (id INTEGER PRIMARY KEY, name TEXT, email TEXT)")
        batch_insert(conn, "test", gen_email_rows(1000))

    def sl_teardown(conn):
        safe_drop(conn, "test")

    def sl_suffix(conn, state):
        execute(conn, "SELECT * FROM test WHERE email LIKE '%@gmail.com'")

    def sl_prefix(conn, state):
        execute(conn, "SELECT * FROM test WHERE name LIKE 'user_1%'")

    benchmarks.append(("select_like_suffix", sl_setup, sl_suffix, sl_teardown))
    benchmarks.append(("select_like_prefix", sl_setup, sl_prefix, sl_teardown))

    # 9-10. select order by / order by + limit
    def so_setup(conn):
        safe_drop(conn, "test")
        execute(conn, "CREATE TABLE test (id INTEGER PRIMARY KEY, name TEXT, value FLOAT)")
        batch_insert(conn, "test", gen_basic_rows(1000))

    def so_teardown(conn):
        safe_drop(conn, "test")

    def so_order(conn, state):
        execute(conn, "SELECT * FROM test ORDER BY value DESC")

    def so_limit(conn, state):
        execute(conn, "SELECT * FROM test ORDER BY value DESC LIMIT 10")

    benchmarks.append(("select_order_by", so_setup, so_order, so_teardown))
    benchmarks.append(("select_order_limit", so_setup, so_limit, so_teardown))

    # 11-12. update single / multi
    def up_setup(conn):
        safe_drop(conn, "test")
        execute(conn, "CREATE TABLE test (id INTEGER PRIMARY KEY, name TEXT, value FLOAT, active BOOLEAN)")
        batch_insert(conn, "test", gen_update_rows(1000))

    def up_teardown(conn):
        safe_drop(conn, "test")

    def up_single(conn, state):
        i = state["counter"] % 1000
        execute(conn, f"UPDATE test SET value = {state['counter'] * 1.0} WHERE id = {i}")

    def up_multi(conn, state):
        threshold = (state["counter"] % 500) * 1.0
        execute(conn, f"UPDATE test SET active = FALSE WHERE value < {threshold}")

    benchmarks.append(("update_single", up_setup, up_single, up_teardown))
    benchmarks.append(("update_multi", up_setup, up_multi, up_teardown))

    # 13. delete single (10K rows, consumed during bench)
    def del_setup(conn):
        safe_drop(conn, "test")
        execute(conn, "CREATE TABLE test (id INTEGER PRIMARY KEY, name TEXT)")
        batch_insert(conn, "test", gen_delete_rows(10000))

    def del_teardown(conn):
        safe_drop(conn, "test")

    def del_bench(conn, state):
        execute(conn, f"DELETE FROM test WHERE id = {state['counter']}")

    benchmarks.append(("delete_single", del_setup, del_bench, del_teardown))

    # 14. mixed workload
    def mx_setup(conn):
        safe_drop(conn, "products")
        execute(conn, "CREATE TABLE products (id INTEGER PRIMARY KEY, name TEXT, price FLOAT, stock INTEGER)")
        batch_insert(conn, "products", gen_product_rows(500))

    def mx_teardown(conn):
        safe_drop(conn, "products")

    def mx_bench(conn, state):
        counter = 500 + state["counter"]
        op = counter % 20
        if op < 12:
            if op < 4:
                execute(conn, "SELECT * FROM products WHERE price > 100")
            elif op < 8:
                execute(conn, f"SELECT * FROM products WHERE id = {counter % 500}")
            else:
                execute(conn, "SELECT * FROM products ORDER BY price DESC LIMIT 10")
        elif op < 16:
            execute(conn, f"UPDATE products SET stock = {counter} WHERE id = {counter % 500}")
        elif op < 19:
            execute(conn, f"INSERT INTO products VALUES ({counter}, 'new_product_{counter}', {counter % 1000}.99, {counter * 5})")
        else:
            execute(conn, f"DELETE FROM products WHERE id = {counter}")

    benchmarks.append(("mixed_workload", mx_setup, mx_bench, mx_teardown))

    # 15-17. table sizes 100 / 1K / 10K
    for size in [100, 1000, 10000]:
        def make_ts(sz):
            def ts_setup(conn):
                safe_drop(conn, "test")
                execute(conn, "CREATE TABLE test (id INTEGER PRIMARY KEY, name TEXT, value FLOAT)")
                batch_insert(conn, "test", gen_basic_rows(sz))

            def ts_bench(conn, state):
                execute(conn, "SELECT * FROM test WHERE value > 50")

            def ts_teardown(conn):
                safe_drop(conn, "test")

            return ts_setup, ts_bench, ts_teardown

        s, b, t = make_ts(size)
        benchmarks.append((f"table_size_{size}", s, b, t))

    # 18-19. complex queries (two tables)
    def cq_setup(conn):
        safe_drop(conn, "users")
        safe_drop(conn, "orders")
        execute(conn, "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, age INTEGER, active BOOLEAN)")
        execute(conn, "CREATE TABLE orders (id INTEGER PRIMARY KEY, user_id INTEGER, amount FLOAT, status TEXT)")
        batch_insert(conn, "users", gen_user_rows(500))
        batch_insert(conn, "orders", gen_order_rows(2000))

    def cq_teardown(conn):
        safe_drop(conn, "orders")
        safe_drop(conn, "users")

    def cq_and_or(conn, state):
        execute(conn, "SELECT * FROM orders WHERE status = 'completed' AND amount > 500")

    def cq_multi(conn, state):
        execute(conn, "SELECT * FROM users WHERE age > 30 AND active = TRUE")

    benchmarks.append(("complex_and_or", cq_setup, cq_and_or, cq_teardown))
    benchmarks.append(("complex_multi", cq_setup, cq_multi, cq_teardown))

    return benchmarks


# ---------------------------------------------------------------------------
# Output formatting
# ---------------------------------------------------------------------------

def fmt_ms(val):
    if val < 0.01:
        return f"{val * 1000:.1f}us"
    if val < 1.0:
        return f"{val:.3f}"
    return f"{val:.2f}"


def print_comparison(rustydb_results, mysql_results, args):
    w = 80
    print()
    print("=" * w)
    print("  RustyDB vs MySQL  --  Benchmark Comparison")
    print(f"  Iterations: {args.iterations}   Warmup: {args.warmup}")
    if rustydb_results:
        print(f"  RustyDB: {args.rustydb_host}:{args.rustydb_port}")
    if mysql_results:
        print(f"  MySQL:   {args.mysql_host}:{args.mysql_port}")
    print("=" * w)
    print()

    have_both = bool(rustydb_results) and bool(mysql_results)

    header_bench = "Benchmark"
    header_r = "RustyDB (ms)"
    header_m = "MySQL (ms)"
    header_ratio = "Ratio"

    if have_both:
        print(f"  {header_bench:<30s}  {header_r:>14s}  {header_m:>14s}  {header_ratio:>10s}")
        print("  " + "-" * (w - 4))
    elif rustydb_results:
        print(f"  {header_bench:<30s}  {header_r:>14s}")
        print("  " + "-" * 48)
    else:
        print(f"  {header_bench:<30s}  {header_m:>14s}")
        print("  " + "-" * 48)

    all_names = list(dict.fromkeys(
        [r.name for r in (rustydb_results or [])] +
        [r.name for r in (mysql_results or [])]
    ))

    r_map = {r.name: r for r in (rustydb_results or [])}
    m_map = {r.name: r for r in (mysql_results or [])}

    for name in all_names:
        r = r_map.get(name)
        m = m_map.get(name)

        if have_both and r and m:
            ratio = m.median / r.median if r.median > 0 else float("inf")
            faster = "rustydb" if ratio > 1 else "mysql"
            ratio_val = ratio if ratio >= 1 else 1 / ratio
            marker = "<" if faster == "mysql" else ""
            print(f"  {name:<30s}  {fmt_ms(r.median):>14s}  {fmt_ms(m.median):>14s}  {ratio_val:>8.1f}x {marker}")
        elif r:
            print(f"  {name:<30s}  {fmt_ms(r.median):>14s}")
        elif m:
            if have_both:
                print(f"  {name:<30s}  {'--':>14s}  {fmt_ms(m.median):>14s}")
            else:
                print(f"  {name:<30s}  {fmt_ms(m.median):>14s}")

    print()

    if have_both:
        print("  Ratio = MySQL median / RustyDB median  (higher = RustyDB faster)")
        print("  '<' marks benchmarks where MySQL was faster")
        print()

    print("Detailed Statistics")
    print("-" * w)

    for name in all_names:
        r = r_map.get(name)
        m = m_map.get(name)
        print(f"\n  {name}")
        if r:
            print(f"    RustyDB  mean={fmt_ms(r.mean):>10s}  median={fmt_ms(r.median):>10s}  "
                  f"min={fmt_ms(r.min):>10s}  max={fmt_ms(r.max):>10s}  stdev={fmt_ms(r.stdev):>10s}")
        if m:
            print(f"    MySQL    mean={fmt_ms(m.mean):>10s}  median={fmt_ms(m.median):>10s}  "
                  f"min={fmt_ms(m.min):>10s}  max={fmt_ms(m.max):>10s}  stdev={fmt_ms(m.stdev):>10s}")

    print()


def write_csv(path, rustydb_results, mysql_results):
    all_names = list(dict.fromkeys(
        [r.name for r in (rustydb_results or [])] +
        [r.name for r in (mysql_results or [])]
    ))
    r_map = {r.name: r for r in (rustydb_results or [])}
    m_map = {r.name: r for r in (mysql_results or [])}

    with open(path, "w", newline="") as f:
        w = csv.writer(f)
        w.writerow([
            "benchmark",
            "rustydb_median_ms", "rustydb_mean_ms", "rustydb_min_ms", "rustydb_max_ms", "rustydb_stdev_ms",
            "mysql_median_ms", "mysql_mean_ms", "mysql_min_ms", "mysql_max_ms", "mysql_stdev_ms",
            "ratio",
        ])
        for name in all_names:
            r = r_map.get(name)
            m = m_map.get(name)
            ratio = ""
            if r and m and r.median > 0:
                ratio = f"{m.median / r.median:.2f}"
            w.writerow([
                name,
                f"{r.median:.4f}" if r else "",
                f"{r.mean:.4f}" if r else "",
                f"{r.min:.4f}" if r else "",
                f"{r.max:.4f}" if r else "",
                f"{r.stdev:.4f}" if r else "",
                f"{m.median:.4f}" if m else "",
                f"{m.mean:.4f}" if m else "",
                f"{m.min:.4f}" if m else "",
                f"{m.max:.4f}" if m else "",
                f"{m.stdev:.4f}" if m else "",
                ratio,
            ])
    print(f"CSV written to {path}")


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main():
    args = parse_args()

    benchmarks = build_benchmarks()

    if args.filter:
        pat = re.compile(args.filter, re.IGNORECASE)
        benchmarks = [(n, s, b, t) for n, s, b, t in benchmarks if pat.search(n)]

    if not benchmarks:
        print("No benchmarks matched the filter.")
        sys.exit(1)

    print()
    print(f"Connecting to targets...")

    rustydb_conn = try_connect(
        args.rustydb_host, args.rustydb_port,
        args.rustydb_user, args.rustydb_pass,
        timeout=10,
    )
    if rustydb_conn:
        print(f"  RustyDB  {args.rustydb_host}:{args.rustydb_port}  connected")
    else:
        print(f"  RustyDB  {args.rustydb_host}:{args.rustydb_port}  not available, skipping")

    mysql_conn = try_connect(
        args.mysql_host, args.mysql_port,
        args.mysql_user, args.mysql_pass,
        database=args.mysql_db,
        timeout=15,
    )
    if mysql_conn:
        print(f"  MySQL    {args.mysql_host}:{args.mysql_port}  connected")
    else:
        print(f"  MySQL    {args.mysql_host}:{args.mysql_port}  not available, skipping")

    if not rustydb_conn and not mysql_conn:
        print("\nNo targets available. Nothing to benchmark.")
        sys.exit(1)

    print(f"\nRunning {len(benchmarks)} benchmarks  (warmup={args.warmup}, iterations={args.iterations})")
    print()

    rustydb_results = []
    mysql_results = []

    for name, setup_fn, bench_fn, teardown_fn in benchmarks:
        sys.stdout.write(f"  {name:<30s} ")
        sys.stdout.flush()

        if rustydb_conn:
            try:
                r = run_bench(rustydb_conn, name, setup_fn, bench_fn, teardown_fn,
                              args.warmup, args.iterations)
                rustydb_results.append(r)
                sys.stdout.write(f" R:{fmt_ms(r.median):>8s}")
            except Exception as e:
                sys.stdout.write(f" R:ERROR")
                print(f"\n    RustyDB error: {e}", file=sys.stderr)

        if mysql_conn:
            try:
                r = run_bench(mysql_conn, name, setup_fn, bench_fn, teardown_fn,
                              args.warmup, args.iterations)
                mysql_results.append(r)
                sys.stdout.write(f" M:{fmt_ms(r.median):>8s}")
            except Exception as e:
                sys.stdout.write(f" M:ERROR")
                print(f"\n    MySQL error: {e}", file=sys.stderr)

        print()

    if rustydb_conn:
        try:
            rustydb_conn.close()
        except Exception:
            pass
    if mysql_conn:
        try:
            mysql_conn.close()
        except Exception:
            pass

    print_comparison(rustydb_results, mysql_results, args)

    if args.csv:
        write_csv(args.csv, rustydb_results, mysql_results)


if __name__ == "__main__":
    main()
