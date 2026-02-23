#!/usr/bin/env python3
"""
RustyDB MySQL Wire Protocol Test Suite

Tests the RustyDB server's MySQL wire protocol v10 implementation using
mysql-connector-python. Validates connection handling, authentication,
system queries, DDL, DML (INSERT/SELECT/UPDATE/DELETE), and cleanup.

Usage:
    python test_wire_python.py [--host HOST] [--port PORT] [--user USER] [--password PASSWORD]

Environment variables (overridden by CLI args):
    RUSTYDB_TEST_HOST      default: 127.0.0.1
    RUSTYDB_WIRE_PORT      default: 3307
    RUSTYDB_USERNAME       default: root
    RUSTYDB_PASSWORD       default: (empty)

Requirements:
    pip install mysql-connector-python

    Fallback alternative (not used by this script):
        pip install PyMySQL
"""

import argparse
import os
import sys
import traceback

try:
    import mysql.connector
    from mysql.connector import Error as MySQLError
except ImportError:
    print("ERROR: mysql-connector-python is not installed.")
    print("Install it with:  pip install mysql-connector-python")
    print()
    print("Alternatively, you can use PyMySQL (requires adapting this script):")
    print("  pip install PyMySQL")
    sys.exit(1)


# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------

TEST_TABLE = "_rustydb_test_wire"


def parse_args():
    parser = argparse.ArgumentParser(
        description="RustyDB MySQL Wire Protocol Test Suite"
    )
    parser.add_argument(
        "--host",
        default=os.environ.get("RUSTYDB_TEST_HOST", "127.0.0.1"),
        help="Server host (default: 127.0.0.1)",
    )
    parser.add_argument(
        "--port",
        type=int,
        default=int(os.environ.get("RUSTYDB_WIRE_PORT", "3307")),
        help="Server port (default: 3307)",
    )
    parser.add_argument(
        "--user",
        default=os.environ.get("RUSTYDB_USERNAME", "root"),
        help="Username (default: root)",
    )
    parser.add_argument(
        "--password",
        default=os.environ.get("RUSTYDB_PASSWORD", ""),
        help="Password (default: empty)",
    )
    return parser.parse_args()


# ---------------------------------------------------------------------------
# Test runner infrastructure
# ---------------------------------------------------------------------------

class TestRunner:
    """Lightweight test runner that tracks pass/fail counts and prints results."""

    def __init__(self, host, port, user, password):
        self.host = host
        self.port = port
        self.user = user
        self.password = password
        self.passed = 0
        self.failed = 0
        self.results = []
        self.conn = None

    def connect(self):
        """Create a new connection to the RustyDB server."""
        return mysql.connector.connect(
            host=self.host,
            port=self.port,
            user=self.user,
            password=self.password,
            connection_timeout=10,
            auth_plugin="mysql_native_password",
            use_pure=True,
        )

    def record_pass(self, name):
        self.passed += 1
        self.results.append((name, True, None))
        print(f"  PASS  {name}")

    def record_fail(self, name, error):
        self.failed += 1
        self.results.append((name, False, str(error)))
        print(f"  FAIL  {name}")
        print(f"        Error: {error}")

    def run_test(self, name, fn):
        """Run a single test function, catching and reporting exceptions."""
        try:
            fn()
            self.record_pass(name)
        except AssertionError as e:
            self.record_fail(name, e)
        except MySQLError as e:
            self.record_fail(name, f"MySQL error: {e}")
        except Exception as e:
            self.record_fail(name, f"{type(e).__name__}: {e}")

    def print_summary(self):
        total = self.passed + self.failed
        print()
        print("=" * 60)
        print(f"  Results: {self.passed}/{total} tests passed")
        if self.failed > 0:
            print(f"  FAILED tests:")
            for name, ok, err in self.results:
                if not ok:
                    print(f"    - {name}: {err}")
        print("=" * 60)

    def exit_code(self):
        return 0 if self.failed == 0 else 1


# ---------------------------------------------------------------------------
# Helper functions
# ---------------------------------------------------------------------------

def execute_query(conn, sql, params=None):
    """Execute a query and return (cursor_description, rows)."""
    cursor = conn.cursor()
    cursor.execute(sql, params)
    if cursor.description is not None:
        rows = cursor.fetchall()
        desc = cursor.description
        cursor.close()
        return desc, rows
    else:
        conn.commit()
        affected = cursor.rowcount
        cursor.close()
        return None, affected


def execute_dml(conn, sql, params=None):
    """Execute a DML statement (INSERT/UPDATE/DELETE) and return rowcount."""
    cursor = conn.cursor()
    cursor.execute(sql, params)
    conn.commit()
    affected = cursor.rowcount
    cursor.close()
    return affected


def fetch_all(conn, sql, params=None):
    """Execute a SELECT and return the list of rows (as tuples)."""
    cursor = conn.cursor()
    cursor.execute(sql, params)
    rows = cursor.fetchall()
    cursor.close()
    return rows


def fetch_one(conn, sql, params=None):
    """Execute a SELECT and return the first row."""
    cursor = conn.cursor()
    cursor.execute(sql, params)
    row = cursor.fetchone()
    # Drain any remaining results to keep connection clean
    try:
        cursor.fetchall()
    except Exception:
        pass
    cursor.close()
    return row


# ---------------------------------------------------------------------------
# Cleanup helper
# ---------------------------------------------------------------------------

def safe_drop_table(conn):
    """Attempt to drop the test table, ignoring errors if it does not exist."""
    try:
        cursor = conn.cursor()
        cursor.execute(f"DROP TABLE {TEST_TABLE}")
        conn.commit()
        cursor.close()
    except Exception:
        pass


# ---------------------------------------------------------------------------
# Test definitions
# ---------------------------------------------------------------------------

def build_tests(runner):
    """Return a list of (test_name, test_function) tuples."""

    tests = []

    # ------------------------------------------------------------------
    # 1. Connection Test
    # ------------------------------------------------------------------
    def test_connection():
        conn = runner.connect()
        assert conn.is_connected(), "Expected connection to be open"
        conn.close()

    tests.append(("Connection", test_connection))

    # ------------------------------------------------------------------
    # 2. Ping Test
    # ------------------------------------------------------------------
    def test_ping():
        conn = runner.connect()
        try:
            conn.ping(reconnect=False)
        finally:
            conn.close()

    tests.append(("Ping", test_ping))

    # ------------------------------------------------------------------
    # 3. System Queries
    # ------------------------------------------------------------------
    def test_select_version():
        conn = runner.connect()
        try:
            row = fetch_one(conn, "SELECT @@version")
            assert row is not None, "Expected a row from SELECT @@version"
            version = str(row[0])
            assert "RustyDB" in version, (
                f"Expected version to contain 'RustyDB', got: {version}"
            )
        finally:
            conn.close()

    tests.append(("System: SELECT @@version", test_select_version))

    def test_select_database():
        conn = runner.connect()
        try:
            row = fetch_one(conn, "SELECT DATABASE()")
            assert row is not None, "Expected a row from SELECT DATABASE()"
            db = str(row[0])
            assert db == "rustydb", f"Expected 'rustydb', got: {db}"
        finally:
            conn.close()

    tests.append(("System: SELECT DATABASE()", test_select_database))

    def test_select_one():
        conn = runner.connect()
        try:
            row = fetch_one(conn, "SELECT 1")
            assert row is not None, "Expected a row from SELECT 1"
            val = int(row[0])
            assert val == 1, f"Expected 1, got: {val}"
        finally:
            conn.close()

    tests.append(("System: SELECT 1", test_select_one))

    def test_show_databases():
        conn = runner.connect()
        try:
            rows = fetch_all(conn, "SHOW DATABASES")
            assert len(rows) >= 1, "Expected at least 1 database"
            db_names = [str(r[0]) for r in rows]
            assert "rustydb" in db_names, (
                f"Expected 'rustydb' in databases, got: {db_names}"
            )
        finally:
            conn.close()

    tests.append(("System: SHOW DATABASES", test_show_databases))

    # ------------------------------------------------------------------
    # 4. DDL: CREATE TABLE
    # ------------------------------------------------------------------
    def test_create_table():
        conn = runner.connect()
        try:
            safe_drop_table(conn)
            cursor = conn.cursor()
            cursor.execute(
                f"CREATE TABLE {TEST_TABLE} ("
                f"  id INTEGER PRIMARY KEY,"
                f"  name TEXT NOT NULL,"
                f"  score FLOAT,"
                f"  active BOOLEAN"
                f")"
            )
            conn.commit()
            cursor.close()

            # Verify the table exists via SHOW TABLES
            rows = fetch_all(conn, "SHOW TABLES")
            table_names = [str(r[0]) for r in rows]
            assert TEST_TABLE in table_names, (
                f"Expected '{TEST_TABLE}' in SHOW TABLES, got: {table_names}"
            )
        finally:
            conn.close()

    tests.append(("DDL: CREATE TABLE", test_create_table))

    # ------------------------------------------------------------------
    # 5. INSERT rows
    # ------------------------------------------------------------------
    def test_insert_single():
        conn = runner.connect()
        try:
            execute_dml(
                conn,
                f"INSERT INTO {TEST_TABLE} (id, name, score, active) "
                f"VALUES (1, 'Alice', 95.5, true)"
            )
            execute_dml(
                conn,
                f"INSERT INTO {TEST_TABLE} (id, name, score, active) "
                f"VALUES (2, 'Bob', 82.3, true)"
            )
            execute_dml(
                conn,
                f"INSERT INTO {TEST_TABLE} (id, name, score, active) "
                f"VALUES (3, 'Charlie', 71.0, false)"
            )
            execute_dml(
                conn,
                f"INSERT INTO {TEST_TABLE} (id, name, score, active) "
                f"VALUES (4, 'Diana', 88.9, true)"
            )
            execute_dml(
                conn,
                f"INSERT INTO {TEST_TABLE} (id, name, score, active) "
                f"VALUES (5, 'Eve', 63.2, false)"
            )

            # Verify row count
            rows = fetch_all(conn, f"SELECT * FROM {TEST_TABLE}")
            assert len(rows) == 5, f"Expected 5 rows after inserts, got {len(rows)}"
        finally:
            conn.close()

    tests.append(("INSERT: single-row inserts", test_insert_single))

    # ------------------------------------------------------------------
    # 6. SELECT tests
    # ------------------------------------------------------------------
    def test_select_all():
        conn = runner.connect()
        try:
            rows = fetch_all(conn, f"SELECT * FROM {TEST_TABLE}")
            assert len(rows) == 5, f"Expected 5 rows, got {len(rows)}"
        finally:
            conn.close()

    tests.append(("SELECT: all rows (SELECT *)", test_select_all))

    def test_select_specific_columns():
        conn = runner.connect()
        try:
            desc, rows = execute_query(
                conn, f"SELECT name, score FROM {TEST_TABLE}"
            )
            assert len(rows) == 5, f"Expected 5 rows, got {len(rows)}"
            # Each row should have exactly 2 columns
            for row in rows:
                assert len(row) == 2, (
                    f"Expected 2 columns per row, got {len(row)}: {row}"
                )
        finally:
            conn.close()

    tests.append(("SELECT: specific columns", test_select_specific_columns))

    def test_select_where_eq():
        conn = runner.connect()
        try:
            rows = fetch_all(
                conn, f"SELECT * FROM {TEST_TABLE} WHERE name = 'Alice'"
            )
            assert len(rows) == 1, f"Expected 1 row for name='Alice', got {len(rows)}"
            # Verify the name column value (second column, index 1)
            assert str(rows[0][1]) == "Alice", (
                f"Expected name='Alice', got '{rows[0][1]}'"
            )
        finally:
            conn.close()

    tests.append(("SELECT: WHERE = (equality)", test_select_where_eq))

    def test_select_where_gt():
        conn = runner.connect()
        try:
            rows = fetch_all(
                conn, f"SELECT * FROM {TEST_TABLE} WHERE score > 85.0"
            )
            # Alice (95.5) and Diana (88.9) should match
            assert len(rows) == 2, (
                f"Expected 2 rows with score > 85.0, got {len(rows)}"
            )
        finally:
            conn.close()

    tests.append(("SELECT: WHERE > (greater than)", test_select_where_gt))

    def test_select_where_lt():
        conn = runner.connect()
        try:
            rows = fetch_all(
                conn, f"SELECT * FROM {TEST_TABLE} WHERE score < 75.0"
            )
            # Charlie (71.0) and Eve (63.2) should match
            assert len(rows) == 2, (
                f"Expected 2 rows with score < 75.0, got {len(rows)}"
            )
        finally:
            conn.close()

    tests.append(("SELECT: WHERE < (less than)", test_select_where_lt))

    def test_select_where_like():
        conn = runner.connect()
        try:
            rows = fetch_all(
                conn, f"SELECT * FROM {TEST_TABLE} WHERE name LIKE 'A%'"
            )
            assert len(rows) >= 1, (
                f"Expected at least 1 row matching LIKE 'A%', got {len(rows)}"
            )
            for row in rows:
                assert str(row[1]).startswith("A"), (
                    f"Expected name starting with 'A', got '{row[1]}'"
                )
        finally:
            conn.close()

    tests.append(("SELECT: WHERE LIKE", test_select_where_like))

    def test_select_order_by_asc():
        conn = runner.connect()
        try:
            rows = fetch_all(
                conn, f"SELECT * FROM {TEST_TABLE} ORDER BY score ASC"
            )
            assert len(rows) == 5, f"Expected 5 rows, got {len(rows)}"
            scores = [float(r[2]) for r in rows]
            assert scores == sorted(scores), (
                f"Expected ascending order, got: {scores}"
            )
        finally:
            conn.close()

    tests.append(("SELECT: ORDER BY ASC", test_select_order_by_asc))

    def test_select_order_by_desc():
        conn = runner.connect()
        try:
            rows = fetch_all(
                conn, f"SELECT * FROM {TEST_TABLE} ORDER BY score DESC"
            )
            assert len(rows) == 5, f"Expected 5 rows, got {len(rows)}"
            scores = [float(r[2]) for r in rows]
            assert scores == sorted(scores, reverse=True), (
                f"Expected descending order, got: {scores}"
            )
        finally:
            conn.close()

    tests.append(("SELECT: ORDER BY DESC", test_select_order_by_desc))

    def test_select_limit():
        conn = runner.connect()
        try:
            rows = fetch_all(
                conn, f"SELECT * FROM {TEST_TABLE} LIMIT 3"
            )
            assert len(rows) == 3, f"Expected 3 rows with LIMIT 3, got {len(rows)}"
        finally:
            conn.close()

    tests.append(("SELECT: LIMIT", test_select_limit))

    def test_select_combined():
        conn = runner.connect()
        try:
            rows = fetch_all(
                conn,
                f"SELECT name, score FROM {TEST_TABLE} "
                f"WHERE score > 70.0 ORDER BY score DESC LIMIT 2"
            )
            assert len(rows) == 2, (
                f"Expected 2 rows for combined query, got {len(rows)}"
            )
            scores = [float(r[1]) for r in rows]
            assert scores == sorted(scores, reverse=True), (
                f"Expected descending order, got: {scores}"
            )
            for s in scores:
                assert s > 70.0, f"Expected score > 70.0, got {s}"
        finally:
            conn.close()

    tests.append(("SELECT: WHERE + ORDER BY + LIMIT", test_select_combined))

    # ------------------------------------------------------------------
    # 7. UPDATE
    # ------------------------------------------------------------------
    def test_update():
        conn = runner.connect()
        try:
            affected = execute_dml(
                conn,
                f"UPDATE {TEST_TABLE} SET score = 99.9 WHERE name = 'Alice'"
            )
            # Verify the change
            rows = fetch_all(
                conn, f"SELECT score FROM {TEST_TABLE} WHERE name = 'Alice'"
            )
            assert len(rows) == 1, f"Expected 1 row for Alice, got {len(rows)}"
            updated_score = float(rows[0][0])
            assert abs(updated_score - 99.9) < 0.01, (
                f"Expected score ~ 99.9, got {updated_score}"
            )
        finally:
            conn.close()

    tests.append(("UPDATE: modify row with WHERE", test_update))

    # ------------------------------------------------------------------
    # 8. DELETE
    # ------------------------------------------------------------------
    def test_delete():
        conn = runner.connect()
        try:
            # Delete Eve (id=5)
            execute_dml(
                conn,
                f"DELETE FROM {TEST_TABLE} WHERE name = 'Eve'"
            )

            # Verify Eve is gone
            rows = fetch_all(
                conn, f"SELECT * FROM {TEST_TABLE} WHERE name = 'Eve'"
            )
            assert len(rows) == 0, (
                f"Expected 0 rows for Eve after delete, got {len(rows)}"
            )

            # Verify remaining count
            rows = fetch_all(conn, f"SELECT * FROM {TEST_TABLE}")
            assert len(rows) == 4, (
                f"Expected 4 rows after deleting Eve, got {len(rows)}"
            )
        finally:
            conn.close()

    tests.append(("DELETE: remove row with WHERE", test_delete))

    # ------------------------------------------------------------------
    # 9. DROP TABLE
    # ------------------------------------------------------------------
    def test_drop_table():
        conn = runner.connect()
        try:
            cursor = conn.cursor()
            cursor.execute(f"DROP TABLE {TEST_TABLE}")
            conn.commit()
            cursor.close()

            # Verify the table is gone
            rows = fetch_all(conn, "SHOW TABLES")
            table_names = [str(r[0]) for r in rows]
            assert TEST_TABLE not in table_names, (
                f"Expected '{TEST_TABLE}' to be dropped, but still in: {table_names}"
            )
        finally:
            conn.close()

    tests.append(("DDL: DROP TABLE", test_drop_table))

    # ------------------------------------------------------------------
    # 10. Cleanup (ensure no leftover test table)
    # ------------------------------------------------------------------
    def test_cleanup():
        conn = runner.connect()
        try:
            safe_drop_table(conn)
            rows = fetch_all(conn, "SHOW TABLES")
            table_names = [str(r[0]) for r in rows]
            assert TEST_TABLE not in table_names, (
                f"Cleanup failed: '{TEST_TABLE}' still present"
            )
        finally:
            conn.close()

    tests.append(("Cleanup", test_cleanup))

    return tests


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main():
    args = parse_args()

    print()
    print("=" * 60)
    print("  RustyDB MySQL Wire Protocol Test Suite")
    print("=" * 60)
    print(f"  Host:     {args.host}")
    print(f"  Port:     {args.port}")
    print(f"  User:     {args.user}")
    print(f"  Password: {'(set)' if args.password else '(empty)'}")
    print(f"  Table:    {TEST_TABLE}")
    print("=" * 60)
    print()

    runner = TestRunner(args.host, args.port, args.user, args.password)

    # Verify we can reach the server before running the full suite
    print("Connecting to RustyDB server...")
    try:
        probe = runner.connect()
        probe.close()
        print("Connection successful.")
    except MySQLError as e:
        print(f"FATAL: Cannot connect to RustyDB at {args.host}:{args.port}")
        print(f"  Error: {e}")
        print()
        print("Make sure the RustyDB server is running with the wire protocol enabled.")
        print("  Example:  cargo run -- --wire-port 3307")
        print()
        sys.exit(1)
    except Exception as e:
        print(f"FATAL: Unexpected error connecting to {args.host}:{args.port}")
        print(f"  {type(e).__name__}: {e}")
        traceback.print_exc()
        sys.exit(1)

    print()
    print("Running tests...")
    print("-" * 60)

    tests = build_tests(runner)
    for name, fn in tests:
        runner.run_test(name, fn)

    print("-" * 60)
    runner.print_summary()

    sys.exit(runner.exit_code())


if __name__ == "__main__":
    main()
