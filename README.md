# RustyDB

A lightweight, memory-first database written in Rust with a focused advanced SQL engine, MySQL wire protocol compatibility, and crash-safe persistence. RustyDB combines the simplicity of an embedded database with familiar MySQL syntax.

Connect with any standard MySQL client -- `mysql` CLI, Python (`mysql-connector-python`, `PyMySQL`), Rust (`mysql`, `mysql_async`), Node.js, and more.

## Project Goals

- **Memory-first architecture** with full SQL support
- **MySQL-compatible syntax and wire protocol** for drop-in client compatibility
- **Lock-free concurrent access** for maximum throughput
- **Crash-safe persistence** via write-ahead logging
- **Dual-mode operation** - Key-Value store + SQL database
- **Written in Rust** for safety and efficiency

## Features

### Current Beta Release (v0.3.0-beta)

#### Key-Value Store
- Lock-free concurrent access with DashMap
- Zero-copy reads using `Arc<String>`
- Batch operations (`get_many`, `set_many`)
- Thread-safe concurrent reads and writes

#### SQL Database Engine
- **MySQL-dialect parser** powered by `sqlparser`
- **Full CRUD operations** (CREATE, INSERT, SELECT, UPDATE, DELETE)
- **INNER, LEFT, and RIGHT JOIN**, with hash joins for equality predicates
- **COUNT, SUM, AVG, MIN, MAX**, `GROUP BY`, and `HAVING`
- **Scalar, IN, EXISTS, and derived-table subqueries**
- **Materialized non-recursive CTEs**
- **Composite B-tree indexes**, `CREATE/DROP INDEX`, `SHOW INDEXES`, and `EXPLAIN`
- **Three-valued NULL logic**, qualified columns, aliases, ordering, and limits
- **PRIMARY KEY, UNIQUE, CHECK, and RESTRICT foreign-key constraints**
- **Optimistic serializable transactions** with `BEGIN`, `COMMIT`, and `ROLLBACK`
- **Rule optimizer** with predicate pushdown, projection pruning, constant folding, hash-join selection, index selection, and Top-N sorting
- **Data types**: INTEGER, FLOAT, TEXT, BOOLEAN, NULL
- **Schema management** (SHOW TABLES, DESCRIBE)
- **CLI SQL mode** with `--sql` flag

#### Optimization
- B-tree equality-prefix and final-column range scans
- Per-index Bloom filters with configurable false-positive targets
- Bounded LRU parsed/optimized-plan cache
- Reusable bounded SQL scratch-buffer pool
- Stable row IDs and copy-on-write catalog snapshots

#### Persistence & Recovery
- **Crash-safe persistence with WAL**
- **Background flushing to disk**
- **Automatic crash recovery**
- **Configurable flush intervals**
- **Snapshot compaction**
- **Versioned, checksummed committed-transaction SQL WAL**
- **Atomic single-catalog snapshots**
- **Automatic migration of legacy per-table JSON files**

#### Network Interfaces
- **MySQL Wire Protocol (TCP)** -- connect with any MySQL client on port 3307
- **REST API (HTTP)** -- JSON API on port 8080 for KV and SQL operations
- **Basic Auth** -- optional authentication for both HTTP and wire protocol
- **Docker & Docker Compose** support for deployment

#### CLI Modes
- Key-Value REPL mode (default)
- Interactive SQL REPL mode (`--sql`)
- Server mode (`--server`) with HTTP + TCP

## Quick Start

### Installation

```bash
# Clone the repository
git clone https://github.com/IPandral/RustyDB.git
cd rustydb

# Build (CLI only, no server)
cargo build --release

# Build with server + wire protocol
cargo build --release --features server

# Run in Key-Value mode (default)
cargo run --release

# Run in SQL mode
cargo run --release -- --sql

# Run as server (HTTP + MySQL wire protocol)
cargo run --release --features server -- --server

# Run with custom data directory
cargo run --release -- --data=/path/to/data --sql

# Run in memory-only mode (no persistence)
cargo run --release -- --memory --sql
```

### Server Mode

Start the server to expose both the HTTP REST API and MySQL wire protocol:

```bash
# Start with defaults (HTTP on 8080, MySQL on 3307, no auth)
cargo run --release --features server -- --server

# Or configure via environment variables
RUSTYDB_PORT=8080 \
RUSTYDB_WIRE_PORT=3307 \
RUSTYDB_USERNAME=admin \
RUSTYDB_PASSWORD=secret \
cargo run --release --features server -- --server
```

### Connecting with MySQL Clients

Once the server is running, connect with any standard MySQL client:

```bash
# MySQL CLI
mysql -h 127.0.0.1 -P 3307 -u admin -p

# Then run SQL as normal:
# mysql> CREATE TABLE users (id INT PRIMARY KEY, name TEXT, age INT);
# mysql> INSERT INTO users VALUES (1, 'Alice', 30);
# mysql> SELECT * FROM users;
```

**Python** (`mysql-connector-python`):

```python
import mysql.connector

conn = mysql.connector.connect(
    host='127.0.0.1',
    port=3307,
    user='admin',
    password='secret'
)
cursor = conn.cursor()
cursor.execute("CREATE TABLE users (id INT PRIMARY KEY, name TEXT, age INT)")
cursor.execute("INSERT INTO users VALUES (1, 'Alice', 30)")
cursor.execute("SELECT * FROM users")
for row in cursor.fetchall():
    print(row)
conn.close()
```

**Rust** (`mysql` crate):

```rust
use mysql::*;
use mysql::prelude::*;

let pool = Pool::new("mysql://admin:secret@127.0.0.1:3307/rustydb")?;
let mut conn = pool.get_conn()?;

conn.query_drop("CREATE TABLE users (id INT PRIMARY KEY, name TEXT)")?;
conn.query_drop("INSERT INTO users VALUES (1, 'Alice')")?;

let results: Vec<(i64, String)> =
    conn.query("SELECT id, name FROM users")?;
```

### Docker

```bash
# Build and run with Docker Compose
cp .env.example .env
# Edit .env to set credentials and ports
docker-compose up -d

# Or run directly
docker build -t rustydb .
docker run -p 8080:8080 -p 3307:3307 rustydb
```

### Key-Value Mode Usage

```
RustyDB v0.3.0-beta
Running in Key-Value mode

kv> SET name RustyDB
OK

kv> GET name
RustyDB

kv> MSET user:1 alice user:2 bob user:3 charlie
OK (3 keys set)

kv> MGET user:1 user:2 user:3
user:1: alice
user:2: bob
user:3: charlie
```

### SQL Mode Usage

```
RustyDB v0.3.0-beta
Running in SQL mode

sql> CREATE TABLE users (
       id INTEGER PRIMARY KEY,
       name TEXT NOT NULL,
       email TEXT,
       age INTEGER,
       active BOOLEAN
     );
Table 'users' created

sql> INSERT INTO users VALUES (1, 'Alice', 'alice@example.com', 28, TRUE);
1 row(s) affected

sql> INSERT INTO users VALUES
       (2, 'Bob', 'bob@example.com', 34, TRUE),
       (3, 'Charlie', 'charlie@example.com', 22, FALSE);
2 row(s) affected

sql> SELECT * FROM users WHERE age > 25 AND active = TRUE ORDER BY age DESC;
+----+-------+-------------------+-----+--------+
| id | name  | email             | age | active |
+----+-------+-------------------+-----+--------+
|  2 | Bob   | bob@example.com   |  34 | true   |
|  1 | Alice | alice@example.com |  28 | true   |
+----+-------+-------------------+-----+--------+
2 row(s)

sql> SELECT name, email FROM users WHERE email LIKE '%@example.com' ORDER BY name;
+-------+-------------------+
| name  | email             |
+-------+-------------------+
| Alice | alice@example.com |
| Bob   | bob@example.com   |
+-------+-------------------+
2 row(s)

sql> DESCRIBE users;
Table: users
+----------------+----------+----------+-------------+
| Column         | Type     | Nullable | Primary Key |
+----------------+----------+----------+-------------+
| id             | INTEGER  | NO       | YES         |
| name           | TEXT     | NO       | NO          |
| email          | TEXT     | YES      | NO          |
| age            | INTEGER  | YES      | NO          |
| active         | BOOLEAN  | YES      | NO          |
+----------------+----------+----------+-------------+
```

### Programmatic Usage (Rust API)

#### Key-Value Store

```rust
use rustydb::{KVStore, PersistenceConfig};

// In-memory only
let store = KVStore::new();

// With persistence
let store = KVStore::open("./data")?;

// Sync write (crash-safe)
store.set("key".to_string(), "value".to_string())?;

// Async write (faster, buffered)
store.set_async("key".to_string(), "value".to_string())?;

// Read (zero-copy via Arc)
let value = store.get("key")?;

// Batch operations
let results = store.get_many(&["key1", "key2", "key3"])?;
store.set_many(vec![
    ("key1".to_string(), "value1".to_string()),
    ("key2".to_string(), "value2".to_string()),
])?;
```

#### SQL Database

```rust
use rustydb::{SQLDatabase, ExecutionResult};

// In-memory SQL database
let db = SQLDatabase::new();

// With persistence
let db = SQLDatabase::open("./sql_data")?;

// Execute SQL
let result = db.execute("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)");
match result {
    ExecutionResult::TableCreated(name) => println!("Created: {}", name),
    ExecutionResult::Error(e) => println!("Error: {}", e),
    _ => {}
}

db.execute("INSERT INTO users VALUES (1, 'Alice')");
db.execute("INSERT INTO users VALUES (2, 'Bob')");

if let ExecutionResult::Select(rs) = db.execute("SELECT * FROM users WHERE id > 0") {
    println!("{}", rs.to_table_string());
}
```

## HTTP REST API

When running in server mode, the following endpoints are available:

### Key-Value Endpoints

| Method | Path | Description |
|--------|------|-------------|
| GET | `/kv/:key` | Get a value by key |
| PUT | `/kv/:key` | Set a value (sync, crash-safe) |
| POST | `/kv/:key` | Set a value (async, faster) |
| DELETE | `/kv/:key` | Delete a key |
| POST | `/kv/mget` | Get multiple keys |
| POST | `/kv/mset` | Set multiple key-value pairs |
| GET | `/kv/info` | Store statistics |
| POST | `/kv/clear` | Clear all keys |
| POST | `/kv/snapshot` | Create a snapshot |
| POST | `/kv/flush` | Flush pending writes |

### SQL Endpoints

| Method | Path | Description |
|--------|------|-------------|
| POST | `/sql/execute` | Execute a SQL query |
| GET | `/sql/tables` | List all tables |
| POST | `/sql/save` | Save database to disk |

### Other

| Method | Path | Description |
|--------|------|-------------|
| GET | `/health` | Health check (no auth required) |

Authentication uses HTTP Basic Auth when `RUSTYDB_USERNAME` and `RUSTYDB_PASSWORD` are set. The `/health` endpoint is always accessible without authentication.

## Architecture

### Design Principles

1. **Memory-First**: All hot data lives in RAM for sub-microsecond access
2. **Lock-Free Concurrency**: DashMap for true parallel reads and writes
3. **Zero-Copy Reads**: Values wrapped in `Arc<String>` for pointer-only clones
4. **Crash-Safe Persistence**: Write-ahead log ensures no data loss
5. **Dual-Mode Operation**: Key-Value store + Full SQL engine
6. **MySQL Wire Protocol**: Standard client compatibility without custom drivers

### SQL Engine Architecture

```
                    ┌──────────────────┐
                    │   MySQL Client   │
                    │ (mysql, Python,  │
                    │  Rust, Node.js)  │
                    └────────┬─────────┘
                             │ TCP (port 3307)
                    ┌────────▼─────────┐
                    │   Wire Protocol  │
                    │ (MySQL v10)      │
                    └────────┬─────────┘
                             │
┌─────────────────┐ ┌───────▼──────────┐
│   HTTP REST API │ │   SQL Parser     │
│   (port 8080)   │ │ (MySQL syntax)   │
└────────┬────────┘ └───────┬──────────┘
         │                  │
         │          ┌───────▼──────────┐
         └─────────►│ Query Executor   │
                    │ (In-Memory)      │
                    └───────┬──────────┘
                            │
                    ┌───────▼──────────┐
                    │ Table Engine     │
                    │ (DashMap-based)  │
                    └───────┬──────────┘
                            │
                    ┌───────▼──────────┐
                    │ Persistence      │
                    │ (JSON + WAL)     │
                    └──────────────────┘
```

### Supported SQL

- **DDL**: `CREATE TABLE`, `DROP TABLE`, `CREATE [UNIQUE] INDEX`, `DROP INDEX`, `DESCRIBE`, `SHOW TABLES`, `SHOW INDEXES`, `EXPLAIN`
- **DML**: `INSERT`, `SELECT`, `UPDATE`, `DELETE`
- **Data Types**: `INTEGER`/`INT`, `FLOAT`/`DOUBLE`, `TEXT`/`VARCHAR(n)`, `BOOLEAN`/`BOOL`, `NULL`
- **Queries**: qualified columns, aliases, `INNER`/`LEFT`/`RIGHT JOIN`, derived tables, non-recursive CTEs
- **Subqueries**: uncorrelated scalar, `IN`, and `EXISTS`
- **Aggregates**: `COUNT`, `SUM`, `AVG`, `MIN`, `MAX`, `GROUP BY`, `HAVING`
- **WHERE Clauses**: `=`, `!=`, `<`, `<=`, `>`, `>=`, `LIKE`, `IN`, `IS NULL`, `AND`, `OR`
- **Ordering**: `ORDER BY column [ASC|DESC]`
- **Limits**: `LIMIT n`
- **Transactions**: connection-local `BEGIN`, `COMMIT`, and `ROLLBACK`
- **Constraints**: `PRIMARY KEY`, `NOT NULL`, `UNIQUE`, `CHECK`, composite `FOREIGN KEY` with `RESTRICT`
- **Pattern Matching**: `LIKE` with `%` (any chars) and `_` (single char)

Unsupported in this release: correlated/recursive subqueries, recursive CTEs, window functions, lateral/full joins, foreign-key cascades, and `ALTER TABLE` constraint changes.

### MySQL Wire Protocol

RustyDB implements the MySQL client/server protocol v10, allowing standard MySQL clients to connect directly:

- **Handshake**: Server greeting with `mysql_native_password` authentication
- **Commands**: `COM_QUERY`, `COM_PING`, `COM_QUIT`, `COM_INIT_DB`, `COM_FIELD_LIST`
- **System queries**: `SET`, `USE`, `SELECT @@variables`, `SHOW DATABASES`, transaction control
- **Result encoding**: Text protocol with column definitions and row data
- **Transactions**: each wire connection owns an isolated SQL session
- **Reports as**: `5.7.99-RustyDB-0.3.0-beta`

### Persistence Architecture

RustyDB uses a dual-persistence approach:

#### Key-Value Persistence
- **Write-Ahead Log (WAL)**: Every write appended to `rustydb.wal` with `fsync()`
- **Snapshots**: Compact representation in `rustydb.db` with atomic rename

#### SQL Persistence
- **Catalog snapshot**: tables, schemas, constraints, stable row IDs, and index metadata in `catalog.json`
- **SQL WAL**: versioned and checksummed committed-transaction records in `sql_wal.log`
- **Legacy migration**: old `tables/*.json` files are imported when no catalog exists and are retained for safety

```
rustydb_data/
├── rustydb.wal        # KV write-ahead log
├── rustydb.db         # KV snapshot
├── sql_wal.log        # SQL write-ahead log
├── catalog.json       # Atomic SQL catalog snapshot
└── tables/            # Optional retained v0.2 legacy files
```

## Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `RUSTYDB_HOST` | `0.0.0.0` | Bind address |
| `RUSTYDB_PORT` | `8080` | HTTP API port |
| `RUSTYDB_WIRE_PORT` | `3307` | MySQL wire protocol port |
| `RUSTYDB_USERNAME` | *(none)* | Auth username (both HTTP and wire) |
| `RUSTYDB_PASSWORD` | *(none)* | Auth password (both HTTP and wire) |
| `RUSTYDB_DATA_DIR` | `./rustydb_data` | Data directory for persistence |
| `RUSTYDB_MEMORY_ONLY` | `false` | Memory-only mode (no disk writes) |
| `RUSTYDB_PLAN_CACHE_CAPACITY` | `256` | Parsed/optimized-plan LRU entries |
| `RUSTYDB_MEMORY_POOL_CAPACITY` | `32` | Reusable SQL scratch buffers |
| `RUSTYDB_BLOOM_FALSE_POSITIVE_RATE` | `0.01` | Per-index Bloom-filter target |

## Testing

```bash
# Run all unit tests
cargo test

# Run all tests including wire protocol integration tests
cargo test --features server

# Run wire protocol integration tests only
cargo test --features server --test wire_integration

# Run with output
cargo test -- --nocapture

# Run specific test modules
cargo test kvstore::tests
cargo test persistence::tests
cargo test sql::parser::tests
cargo test sql::executor::tests
cargo test sql::database::tests

# Run Python wire protocol tests (requires running server)
# First: cargo run --release --features server -- --server
# Then:  python tests/test_wire_python.py --host 127.0.0.1 --port 3307

# Run benchmarks
cargo bench
cargo bench --bench kv_bench
cargo bench --bench sql_bench
cargo bench --bench stress_test_bench
```

### Test Coverage

| Suite | Tests | Description |
|-------|-------|-------------|
| KV Store | 9 | Set, get, delete, batch ops, concurrency, persistence, crash recovery |
| Persistence | 4 | WAL log/recover, snapshots, async flush, config |
| SQL Parser | 3 | Constraints, advanced queries, indexes, transactions |
| SQL Executor | 8 | CRUD, indexes, joins, aggregates, subqueries, constraints, NULL logic |
| SQL Types | 3 | Value comparison, LIKE patterns, schema validation |
| SQL Database | 5 | Autocommit, transactions, persistence, WAL recovery, legacy migration |
| Wire Protocol | 43 | Packet encoding, auth, TCP integration, isolation, rollback, conflicts |

**Total: 75 tests** (32 unit/database + 43 wire integration)

## Performance

### Benchmark Environment

- **Measured**: June 21, 2026
- **Hardware**: Apple M3 Pro, 11 cores (5 performance + 6 efficiency), 18GB unified memory
- **Build**: RustyDB `0.3.0-beta`, release profile, in-memory embedded API
- **Method**: Criterion warmup plus 10-30 measured samples; lower is better

### SQL Optimization Performance

| Workload | Median | p95 | Result |
|----------|-------:|----:|--------|
| Sequential composite range, 1K rows | 0.151 ms | 0.155 ms | baseline |
| Indexed composite range, 1K rows | 0.015 ms | 0.015 ms | 10.0x faster |
| Sequential composite range, 10K rows | 1.508 ms | 1.583 ms | baseline |
| Indexed composite range, 10K rows | 0.249 ms | 0.251 ms | 6.1x faster |
| Sequential composite range, 100K rows | 14.975 ms | 15.153 ms | baseline |
| Indexed composite range, 100K rows | 3.407 ms | 3.526 ms | 4.4x faster |
| Hash join + grouped aggregate | 5.766 ms | 6.072 ms | 1K users / 10K orders |
| Materialized CTE + IN subquery | 20.381 ms | 21.417 ms | 1K users / 10K orders |
| Serializable transaction, 10 writes | 2.915 ms | 5.299 ms | atomic commit |
| Constraint-checked insert | 0.381 ms | 0.674 ms | PK + UNIQUE + CHECK + FK |
| Warm plan-cache lookup | 0.0025 ms | 0.0026 ms | 4.4x faster than cold |
| Cold plan parse/optimize | 0.0109 ms | 0.0116 ms | unique SQL text |

### MySQL Wire Performance vs MySQL 8

Measured on the same Apple M3 Pro host using macOS 26.5.1 (`arm64`), Docker 27.4.0, MySQL 8.0.46 (`linux/arm64`), Python 3.12.3, and `mysql-connector-python` 9.6.0. RustyDB used its release build in memory-only mode. MySQL used the repository's benchmark configuration with a tmpfs data directory and relaxed benchmark durability.

The harness first compared every workload's returned rows or affected-row count across both engines. All 22 correctness checks passed before timing. Each result below is from 10 warmups and 100 measured wire-protocol iterations on June 21, 2026; lower is better.

| Workload | RustyDB median / p95 | MySQL median / p95 | Median comparison |
|----------|---------------------:|-------------------:|------------------:|
| CREATE TABLE | 0.288 / 0.593 ms | 4.091 / 4.740 ms | RustyDB 14.2x faster |
| Single-row INSERT | 0.362 / 0.500 ms | 0.875 / 1.031 ms | RustyDB 2.4x faster |
| SELECT all, 1K rows | 3.983 / 5.290 ms | 4.698 / 5.324 ms | RustyDB 1.2x faster |
| Filtered SELECT, equality | 1.680 / 2.239 ms | 1.993 / 3.340 ms | RustyDB 1.2x faster |
| ORDER BY + LIMIT | 0.385 / 0.523 ms | 0.799 / 1.402 ms | RustyDB 2.1x faster |
| SELECT, 100 rows | 0.365 / 0.491 ms | 0.683 / 0.815 ms | RustyDB 1.9x faster |
| SELECT, 1K rows | 3.773 / 4.009 ms | 4.741 / 5.292 ms | RustyDB 1.3x faster |
| Indexed range, 100K rows | 1.920 / 2.376 ms | 1.098 / 1.780 ms | MySQL 1.7x faster |
| JOIN + grouped aggregate | 6.285 / 6.537 ms | 4.315 / 4.788 ms | MySQL 1.5x faster |
| CTE + subquery | 20.801 / 22.096 ms | 6.071 / 6.741 ms | MySQL 3.4x faster |
| Single-row UPDATE | 1.608 / 1.800 ms | 0.904 / 1.021 ms | MySQL 1.8x faster |
| Single-row DELETE, 10K table | 12.351 / 13.006 ms | 0.925 / 1.006 ms | MySQL 13.3x faster |

The full 22-workload raw result set, including mean, minimum, maximum, standard deviation, median, and p95, is stored in [`benches/compare/results-0.3.0-beta-2026-06-21.csv`](benches/compare/results-0.3.0-beta-2026-06-21.csv). These figures are host-specific and should be regenerated for release comparisons on other systems.

### Key-Value Performance

| Operation | Median | p95 |
|-----------|-------:|----:|
| GET | 51.6 ns | 51.9 ns |
| SET | 288.3 ns | 304.2 ns |
| 70/30 mixed workload | 125.7 ns | 127.0 ns |
| 4-thread batch of 1K reads | 69.3 µs | 75.5 µs |

### Running Benchmarks

```bash
# Native Rust benchmarks (in-process, no wire protocol overhead)
cargo bench
cargo bench --bench sql_bench
cargo bench --bench kv_bench

# Wire protocol comparison vs MySQL
# 1. Start MySQL container
docker compose -f benches/compare/docker-compose.mysql.yml up -d

# 2. Start RustyDB server (separate terminal)
cargo run --release --features server -- --server --memory

# 3. Install Python dependencies and run
pip install -r benches/compare/requirements.txt
python benches/compare/bench_compare.py

# Filter to specific benchmarks
python benches/compare/bench_compare.py --filter select

# Export results to CSV
python benches/compare/bench_compare.py --csv results.csv

# Cleanup
docker compose -f benches/compare/docker-compose.mysql.yml down
```

## Roadmap

### Phase 1: Core KV Store -- Complete
- [x] In-memory HashMap with DashMap
- [x] Thread-safe operations
- [x] CLI interface
- [x] Comprehensive tests

### Phase 2: Persistence -- Complete
- [x] Append-only write-ahead log (WAL)
- [x] Background flush to disk
- [x] Crash recovery
- [x] Configurable flush interval
- [x] Snapshot compaction

### Phase 3: SQL Engine -- Complete
- [x] MySQL-compatible SQL parser
- [x] Query execution engine
- [x] Full CRUD operations
- [x] WHERE clauses with AND/OR
- [x] LIKE pattern matching
- [x] ORDER BY with ASC/DESC
- [x] LIMIT for pagination
- [x] Schema management (SHOW TABLES, DESCRIBE)
- [x] SQL persistence with WAL
- [x] Interactive SQL CLI mode

### Phase 4: Server & Networking -- Complete
- [x] HTTP REST API (Axum)
- [x] MySQL wire protocol (TCP)
- [x] Basic Auth (HTTP + wire)
- [x] Docker & Docker Compose
- [x] CI/CD pipeline
- [x] Wire protocol integration tests
- [x] Python client test suite

### Phase 5: Optimization -- Complete
- [x] Lock-free reads (DashMap)
- [x] Zero-copy reads (Arc<String>)
- [x] Batch operations
- [x] Indexing (composite B-tree indexes)
- [x] Memory pooling
- [x] Bloom filters
- [x] LRU parsed/optimized-plan cache
- [x] Query optimization

### Phase 6: Advanced SQL -- Complete
- [x] JOIN operations (INNER JOIN, LEFT JOIN, RIGHT JOIN)
- [x] Subqueries and non-recursive CTEs
- [x] Aggregate functions (COUNT, SUM, AVG, MAX, MIN)
- [x] GROUP BY and HAVING clauses
- [x] Transactions (BEGIN, COMMIT, ROLLBACK)
- [x] Constraints (FOREIGN KEY, UNIQUE, CHECK)

## License

Apache 2.0 - See LICENSE file for details

## Contact

Built by Matthew Revill
