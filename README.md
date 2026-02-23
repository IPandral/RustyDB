# RustyDB

A high-performance, in-memory database written in Rust with **full SQL support**, **MySQL wire protocol compatibility**, and crash-safe persistence. RustyDB combines the speed of in-memory storage with MySQL-compatible SQL syntax, delivering **50-3000x faster** performance than traditional databases for common operations.

Connect with any standard MySQL client -- `mysql` CLI, Python (`mysql-connector-python`, `PyMySQL`), Rust (`mysql`, `mysql_async`), Node.js, and more.

## Project Goals

- **50-3000x faster** than MySQL for SQL operations (CREATE: 1.7us vs 1-5ms, INSERT: 987ns vs 50-200us)
- **Memory-first architecture** with full SQL support
- **MySQL-compatible syntax and wire protocol** for drop-in client compatibility
- **Lock-free concurrent access** for maximum throughput
- **Crash-safe persistence** via write-ahead logging
- **Dual-mode operation** - Key-Value store + SQL database

## Features

### Current In Beta Development (v0.2.0)

#### Key-Value Store
- Lock-free concurrent access with DashMap
- Zero-copy reads using `Arc<String>`
- Batch operations (`get_many`, `set_many`)
- Thread-safe concurrent reads and writes

#### SQL Database Engine
- **MySQL-compatible SQL syntax**
- **Full CRUD operations** (CREATE, INSERT, SELECT, UPDATE, DELETE)
- **Advanced WHERE clauses** with AND/OR operators
- **LIKE pattern matching** with `%` and `_` wildcards
- **ORDER BY with ASC/DESC**
- **LIMIT for result pagination**
- **Data types**: INTEGER, FLOAT, TEXT, BOOLEAN, NULL
- **Schema management** (SHOW TABLES, DESCRIBE)
- **CLI SQL mode** with `--sql` flag

#### Persistence & Recovery
- **Crash-safe persistence with WAL**
- **Background flushing to disk**
- **Automatic crash recovery**
- **Configurable flush intervals**
- **Snapshot compaction**
- **Dual persistence** (KV + SQL tables)

#### Network Interfaces
- **MySQL Wire Protocol (TCP)** -- connect with any MySQL client on port 3307
- **REST API (HTTP)** -- JSON API on port 8080 for KV and SQL operations
- **Basic Auth** -- optional authentication for both HTTP and wire protocol
- **Docker & Docker Compose** support for deployment

#### CLI Modes
- Key-Value REPL mode (default)
- Interactive SQL REPL mode (`--sql`)
- Server mode (`--server`) with HTTP + TCP

### Planned
- JOIN operations (INNER JOIN, LEFT JOIN)
- Indexing (B-tree indexes for faster queries)
- Aggregate functions (COUNT, SUM, AVG, MAX, MIN)
- GROUP BY and HAVING clauses
- Transactions with isolation levels
- Query optimization engine

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
RustyDB v0.2.0
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
RustyDB v0.2.0
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

- **DDL**: `CREATE TABLE`, `DROP TABLE`, `DESCRIBE`, `SHOW TABLES`
- **DML**: `INSERT`, `SELECT`, `UPDATE`, `DELETE`
- **Data Types**: `INTEGER`/`INT`, `FLOAT`/`DOUBLE`, `TEXT`/`VARCHAR(n)`, `BOOLEAN`/`BOOL`, `NULL`
- **WHERE Clauses**: `=`, `!=`, `<`, `<=`, `>`, `>=`, `LIKE`, `AND`, `OR`
- **Ordering**: `ORDER BY column [ASC|DESC]`
- **Limits**: `LIMIT n`
- **Constraints**: `PRIMARY KEY`, `NOT NULL`
- **Pattern Matching**: `LIKE` with `%` (any chars) and `_` (single char)

### MySQL Wire Protocol

RustyDB implements the MySQL client/server protocol v10, allowing standard MySQL clients to connect directly:

- **Handshake**: Server greeting with `mysql_native_password` authentication
- **Commands**: `COM_QUERY`, `COM_PING`, `COM_QUIT`, `COM_INIT_DB`, `COM_FIELD_LIST`
- **System queries**: `SET`, `USE`, `SELECT @@variables`, `SHOW DATABASES`, transaction control
- **Result encoding**: Text protocol with column definitions and row data
- **Reports as**: `5.7.99-RustyDB` (compatible with all standard MySQL clients)

### Persistence Architecture

RustyDB uses a dual-persistence approach:

#### Key-Value Persistence
- **Write-Ahead Log (WAL)**: Every write appended to `rustydb.wal` with `fsync()`
- **Snapshots**: Compact representation in `rustydb.db` with atomic rename

#### SQL Table Persistence
- **Per-Table Storage**: Each table as JSON in `tables/tablename.json`
- **SQL WAL**: Separate `sql_wal.log` replayed on startup for crash recovery

```
rustydb_data/
├── rustydb.wal        # KV write-ahead log
├── rustydb.db         # KV snapshot
├── sql_wal.log        # SQL write-ahead log
└── tables/            # SQL table storage
    ├── users.json
    ├── products.json
    └── orders.json
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
| KV Store | 13 | Set, get, delete, batch ops, concurrency, persistence, crash recovery |
| Persistence | 4 | WAL log/recover, snapshots, async flush, config |
| SQL Parser | 5 | CREATE, INSERT, SELECT, UPDATE, DELETE parsing |
| SQL Executor | 4 | Table operations, WHERE, UPDATE, DELETE |
| SQL Types | 3 | Value comparison, LIKE patterns, schema validation |
| SQL Database | 4 | CRUD, MySQL syntax, persistence, batch execute |
| Wire Protocol | 42 | Packet encoding, auth, handshake, TCP integration, system queries |

**Total: 75 tests** (29 unit + 4 database + 42 wire integration)

## Performance Results

**Benchmark Environment**: 
Windows: AMD Ryzen 9 9950X3D (16 cores) with 32GB DDR5 RAM
MacOS: Apple M3 Pro (12 cores) with 18GB unified memory

### SQL Performance vs MySQL

| Operation | RustyDB | MySQL | Speedup |
|-----------|---------|--------|---------|
| CREATE TABLE | 1.7us | 1-5ms | **500-3000x** |
| INSERT | 987ns | 50-200us | **50-200x** |
| SELECT (1K rows) | 60us | 1-10ms | **17-167x** |
| SELECT WHERE | 138us | 200-2000us | **1.4-14x** |
| UPDATE | 54us | 100-1000us | **2-18x** |
| DELETE | 942us | 200-2000us | **~2x** |
| ORDER BY + LIMIT | 5.5us | 1-50ms | **180-9000x** |

### Key-Value Performance

- **Reads (32 threads)**: ~1.1ms for 10K ops (~9M ops/sec)
- **Writes (32 threads)**: ~970us for 10K ops (~10M ops/sec)
- **Memory overhead**: ~24 bytes per key-value pair

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

### Phase 5: Optimization -- In Progress
- [x] Lock-free reads (DashMap)
- [x] Zero-copy reads (Arc<String>)
- [x] Batch operations
- [ ] Indexing (B-tree indexes)
- [ ] Memory pooling
- [ ] Bloom filters
- [ ] LRU eviction
- [ ] Query optimization

### Phase 6: Advanced SQL
- [ ] JOIN operations (INNER JOIN, LEFT JOIN, RIGHT JOIN)
- [ ] Subqueries and CTEs
- [ ] Aggregate functions (COUNT, SUM, AVG, MAX, MIN)
- [ ] GROUP BY and HAVING clauses
- [ ] Transactions (BEGIN, COMMIT, ROLLBACK)
- [ ] Constraints (FOREIGN KEY, UNIQUE, CHECK)

## License

Apache 2.0 - See LICENSE file for details

## Contact

Built by Matthew Revill