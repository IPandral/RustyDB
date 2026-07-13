use rustydb::{KVStore, RUSTYDB_VERSION, SQLDatabase, print_outdated_version_warning};
use std::env;
use std::io::{self, Write};
use std::path::PathBuf;

fn main() {
    let args: Vec<String> = env::args().collect();
    if run_recovery_command(&args) {
        return;
    }
    let use_memory_only = args.iter().any(|a| a == "--memory");
    let use_sql_mode = args.iter().any(|a| a == "--sql");
    let use_server_mode = args.iter().any(|a| a == "--server");

    let data_dir = args
        .iter()
        .find(|a| a.starts_with("--data="))
        .map(|a| a.trim_start_matches("--data="))
        .unwrap_or("./rustydb_data");

    #[cfg(feature = "server")]
    if use_server_mode {
        run_server_mode(use_memory_only, data_dir);
        return;
    }

    #[cfg(not(feature = "server"))]
    if use_server_mode {
        println!("Server mode requires the 'server' feature.");
        println!("Rebuild with: cargo build --release --features server");
        return;
    }

    println!("RustyDB v{}", RUSTYDB_VERSION);
    println!("A high-performance database with KV and SQL support");
    println!("Type 'help' for available commands\n");
    print_outdated_version_warning();

    if use_sql_mode {
        run_sql_mode(data_dir, use_memory_only);
    } else {
        run_kv_mode(data_dir, use_memory_only);
    }
}

fn run_recovery_command(args: &[String]) -> bool {
    let Some(command) = args.get(1).map(String::as_str) else {
        return false;
    };
    if !matches!(command, "backup" | "restore" | "wal-prune") {
        return false;
    }
    let option = |name: &str| {
        args.iter().find_map(|argument| {
            argument
                .strip_prefix(&format!("--{name}="))
                .map(str::to_string)
        })
    };
    let result: Result<String, String> = match command {
        "backup" => (|| {
            let data = option("data").unwrap_or_else(|| "./rustydb_data".to_string());
            let output =
                option("output").ok_or_else(|| "backup requires --output=DIR".to_string())?;
            let manifest = rustydb::BackupManager::create(data, output)?;
            Ok(format!(
                "Backup complete at recovery point sql:{},kv:{}",
                manifest.recovery_point.sql_commit_version, manifest.recovery_point.kv_sequence
            ))
        })(),
        "restore" => (|| {
            let backup =
                option("backup").ok_or_else(|| "restore requires --backup=DIR".to_string())?;
            let output =
                option("output").ok_or_else(|| "restore requires --output=DIR".to_string())?;
            let archive = option("wal-archive").map(PathBuf::from);
            if option("target-time").is_some() && option("target-point").is_some() {
                return Err("Choose only one of --target-time or --target-point".to_string());
            }
            let target = if let Some(timestamp) = option("target-time") {
                rustydb::RecoveryTarget::Timestamp(rustydb::parse_rfc3339(&timestamp)?)
            } else if let Some(point) = option("target-point") {
                rustydb::RecoveryTarget::Point(point.parse()?)
            } else {
                rustydb::RecoveryTarget::Latest
            };
            let point =
                rustydb::BackupManager::restore(backup, archive.as_deref(), output, target)?;
            Ok(format!(
                "Restore complete at recovery point sql:{},kv:{}",
                point.sql_commit_version, point.kv_sequence
            ))
        })(),
        "wal-prune" => (|| {
            let data = option("data").unwrap_or_else(|| "./rustydb_data".to_string());
            let before = option("before")
                .ok_or_else(|| "wal-prune requires --before=RFC3339".to_string())?;
            let apply = args.iter().any(|argument| argument == "--yes");
            let report =
                rustydb::BackupManager::prune(data, rustydb::parse_rfc3339(&before)?, apply)?;
            Ok(format!(
                "{} {} WAL segment(s), {} bytes{}",
                if apply { "Pruned" } else { "Would prune" },
                report.files.len(),
                report.bytes,
                if apply {
                    ""
                } else {
                    " (dry run; pass --yes to apply)"
                }
            ))
        })(),
        _ => unreachable!(),
    };
    match result {
        Ok(message) => println!("{message}"),
        Err(error) => eprintln!("Error: {error}"),
    }
    true
}

#[cfg(feature = "server")]
fn run_server_mode(memory_only: bool, data_dir: &str) {
    use rustydb::{ServerConfig, start_server};

    let mut config = ServerConfig::from_env();
    if memory_only {
        config.memory_only = true;
    }
    if data_dir != "./rustydb_data" {
        config.data_dir = data_dir.to_string();
    }

    let rt = tokio::runtime::Runtime::new().expect("Failed to create Tokio runtime");
    rt.block_on(async {
        if let Err(e) = start_server(config).await {
            eprintln!("Server error: {}", e);
        }
    });
}

/// Run in SQL mode
fn run_sql_mode(data_dir: &str, memory_only: bool) {
    println!("Running in SQL mode");

    let db = if memory_only {
        println!("Using in-memory database (no persistence)\n");
        SQLDatabase::new()
    } else {
        println!("Data directory: {}", data_dir);
        match SQLDatabase::open(data_dir) {
            Ok(db) => {
                let tables = db.list_tables();
                if !tables.is_empty() {
                    println!("Loaded {} table(s): {}", tables.len(), tables.join(", "));
                }
                println!();
                db
            }
            Err(e) => {
                println!("Failed to open database: {}", e);
                println!("Falling back to in-memory mode\n");
                SQLDatabase::new()
            }
        }
    };

    let mut multiline_buffer = String::new();
    let mut in_multiline = false;
    let session = db.session();

    loop {
        if in_multiline {
            print!("      -> ");
        } else {
            print!("sql> ");
        }
        io::stdout().flush().unwrap();

        let mut input = String::new();
        io::stdin()
            .read_line(&mut input)
            .expect("Failed to read line");

        let input = input.trim();

        if input.is_empty() && !in_multiline {
            continue;
        }

        let input_lower = input.to_lowercase();
        if !in_multiline {
            match input_lower.as_str() {
                "exit" | "quit" | "\\q" => {
                    if let Err(e) = db.save() {
                        println!("Warning: Failed to save: {}", e);
                    }
                    println!("Data saved. Goodbye!");
                    break;
                }
                "help" | "\\h" | "\\?" => {
                    print_sql_help();
                    continue;
                }
                "tables" | "\\dt" => {
                    let tables = db.list_tables();
                    if tables.is_empty() {
                        println!("No tables");
                    } else {
                        println!("Tables:");
                        for t in tables {
                            println!("  {}", t);
                        }
                    }
                    continue;
                }
                "save" | "checkpoint" => {
                    match db.save() {
                        Ok(_) => println!("Database saved"),
                        Err(e) => println!("Error: {}", e),
                    }
                    continue;
                }
                "kv" | "kvmode" => {
                    println!("Switching to KV mode not supported in this session.");
                    println!("Restart with --sql flag removed to use KV mode.");
                    continue;
                }
                _ => {}
            }
        }

        // Multi-line SQL: accumulate until we see a terminator
        multiline_buffer.push_str(input);
        multiline_buffer.push(' ');

        let trimmed = multiline_buffer.trim();
        let is_complete = trimmed.ends_with(';')
            || trimmed.to_uppercase().starts_with("SHOW ")
            || trimmed.to_uppercase().starts_with("DESCRIBE ")
            || trimmed.to_uppercase().starts_with("DESC ");

        if !is_complete && !input.is_empty() {
            in_multiline = true;
            continue;
        }

        in_multiline = false;
        let sql = multiline_buffer.trim().to_string();
        multiline_buffer.clear();

        if sql.is_empty() {
            continue;
        }

        let result = session.execute(&sql);
        println!("{result}");
    }
}

/// Run in Key-Value mode
fn run_kv_mode(data_dir: &str, memory_only: bool) {
    println!("Running in Key-Value mode");
    println!("(Use --sql flag for SQL mode)\n");

    let store = if memory_only {
        println!("Running in memory-only mode (no persistence)");
        KVStore::new()
    } else {
        println!("Data directory: {}", data_dir);
        match KVStore::open(data_dir) {
            Ok(store) => {
                let count = store.len().unwrap_or(0);
                if count > 0 {
                    println!("Recovered {} keys from disk", count);
                }
                store
            }
            Err(e) => {
                println!("Failed to open persistent store: {}", e);
                println!("Falling back to memory-only mode");
                KVStore::new()
            }
        }
    };

    println!();

    loop {
        print!("kv> ");
        io::stdout().flush().unwrap();

        let mut input = String::new();
        io::stdin()
            .read_line(&mut input)
            .expect("Failed to read line");

        let input = input.trim();

        if input.is_empty() {
            continue;
        }

        let parts: Vec<&str> = input.splitn(3, ' ').collect();
        let command = parts[0].to_lowercase();

        match command.as_str() {
            "set" => {
                if parts.len() < 3 {
                    println!("Error: SET requires a key and value");
                    println!("Usage: SET <key> <value>");
                    continue;
                }
                let key = parts[1].to_string();
                let value = parts[2].to_string();

                match store.set(key.clone(), value) {
                    Ok(_) => println!("OK"),
                    Err(e) => println!("Error: {}", e),
                }
            }

            "get" => {
                if parts.len() < 2 {
                    println!("Error: GET requires a key");
                    println!("Usage: GET <key>");
                    continue;
                }
                let key = parts[1];

                match store.get(key) {
                    Ok(Some(value)) => println!("{}", value.as_ref()),
                    Ok(None) => println!("(nil)"),
                    Err(e) => println!("Error: {}", e),
                }
            }

            "del" | "delete" => {
                if parts.len() < 2 {
                    println!("Error: DELETE requires a key");
                    println!("Usage: DELETE <key>");
                    continue;
                }
                let key = parts[1];

                match store.delete(key) {
                    Ok(true) => println!("OK (deleted)"),
                    Ok(false) => println!("OK (key not found)"),
                    Err(e) => println!("Error: {}", e),
                }
            }

            "len" | "size" => match store.len() {
                Ok(count) => println!("{}", count),
                Err(e) => println!("Error: {}", e),
            },

            "clear" => match store.clear() {
                Ok(_) => println!("OK (all keys deleted)"),
                Err(e) => println!("Error: {}", e),
            },

            "help" => {
                print_kv_help();
            }

            "setasync" | "set_async" => {
                if parts.len() < 3 {
                    println!("Error: SETASYNC requires a key and value");
                    println!("Usage: SETASYNC <key> <value>");
                    continue;
                }
                let key = parts[1].to_string();
                let value = parts[2].to_string();

                match store.set_async(key, value) {
                    Ok(_) => println!("OK (queued for async write)"),
                    Err(e) => println!("Error: {}", e),
                }
            }

            "mget" | "getmany" | "get_many" => {
                if parts.len() < 2 {
                    println!("Error: MGET requires at least one key");
                    println!("Usage: MGET <key1> <key2> ...");
                    continue;
                }
                let all_parts: Vec<&str> = input.split_whitespace().collect();
                let keys: Vec<&str> = all_parts[1..].to_vec();

                match store.get_many(&keys) {
                    Ok(results) => {
                        if results.is_empty() {
                            println!("(empty - no keys found)");
                        } else {
                            for (key, value) in results {
                                println!("{}: {}", key, value.as_ref());
                            }
                        }
                    }
                    Err(e) => println!("Error: {}", e),
                }
            }

            "mset" | "setmany" | "set_many" => {
                let all_parts: Vec<&str> = input.split_whitespace().collect();
                if all_parts.len() < 3 || !(all_parts.len() - 1).is_multiple_of(2) {
                    println!("Error: MSET requires key-value pairs");
                    println!("Usage: MSET <key1> <value1> <key2> <value2> ...");
                    continue;
                }

                let mut pairs: Vec<(String, String)> = Vec::new();
                let mut i = 1;
                while i < all_parts.len() {
                    pairs.push((all_parts[i].to_string(), all_parts[i + 1].to_string()));
                    i += 2;
                }

                match store.set_many(pairs) {
                    Ok(count) => println!("OK ({} keys set)", count),
                    Err(e) => println!("Error: {}", e),
                }
            }

            "isempty" | "is_empty" | "empty" => match store.is_empty() {
                Ok(true) => println!("true (store is empty)"),
                Ok(false) => println!("false (store has {} keys)", store.len().unwrap_or(0)),
                Err(e) => println!("Error: {}", e),
            },

            "snapshot" => match store.snapshot() {
                Ok(_) => println!("OK (snapshot created, WAL compacted)"),
                Err(e) => println!("Error: {}", e),
            },

            "flush" => match store.flush() {
                Ok(_) => println!("OK (all pending writes flushed)"),
                Err(e) => println!("Error: {}", e),
            },

            "walsize" => match store.wal_size() {
                Ok(size) => {
                    if size < 1024 {
                        println!("{} bytes", size);
                    } else if size < 1024 * 1024 {
                        println!("{:.2} KB", size as f64 / 1024.0);
                    } else {
                        println!("{:.2} MB", size as f64 / (1024.0 * 1024.0));
                    }
                }
                Err(e) => println!("Error: {}", e),
            },

            "sql" | "sqlmode" => {
                println!("Switching to SQL mode not supported in this session.");
                println!("Restart with --sql flag to use SQL mode.");
            }

            "exit" | "quit" => {
                let _ = store.flush();
                println!("Data saved. Goodbye!");
                break;
            }

            _ => {
                println!("Unknown command: '{}'", command);
                println!("Type 'help' for available commands");
            }
        }
    }
}

fn print_sql_help() {
    println!("\n  RustyDB SQL Mode");
    println!("  =================\n");
    println!("  SQL Commands:");
    println!("    CREATE TABLE name (col1 TYPE, col2 TYPE, ...)");
    println!("    CREATE [UNIQUE] INDEX name ON table (col1, ...)");
    println!("    DROP INDEX name ON table");
    println!("    DROP TABLE name");
    println!("    INSERT INTO table VALUES (val1, val2, ...)");
    println!("    SELECT ... FROM ... [JOIN ...] [WHERE ...] [GROUP BY ...] [HAVING ...]");
    println!("    UPDATE table SET col = val [WHERE ...]");
    println!("    DELETE FROM table [WHERE ...]");
    println!("    SHOW TABLES");
    println!("    SHOW INDEXES FROM table");
    println!("    DESCRIBE table");
    println!("    EXPLAIN SELECT ...");
    println!("    BEGIN, COMMIT, ROLLBACK");
    println!();
    println!("  Data Types:");
    println!("    INTEGER, INT        - Integer numbers");
    println!("    FLOAT, DOUBLE       - Floating point numbers");
    println!("    TEXT, VARCHAR(n)    - Text strings");
    println!("    BOOLEAN, BOOL       - TRUE or FALSE");
    println!();
    println!("  Column Modifiers:");
    println!("    PRIMARY KEY         - Unique identifier");
    println!("    UNIQUE              - Unique column or column group");
    println!("    CHECK (expression)  - Row validation");
    println!("    FOREIGN KEY (...) REFERENCES table (...) - Referential integrity");
    println!("    NOT NULL            - Cannot be NULL");
    println!();
    println!("  WHERE Operators:");
    println!("    =, !=, <, <=, >, >= - Comparison");
    println!("    LIKE                - Pattern matching (% and _)");
    println!("    AND, OR             - Logical operators");
    println!();
    println!("  Special Commands:");
    println!("    tables, \\dt         - List all tables");
    println!("    save, checkpoint    - Save database to disk");
    println!("    help, \\h            - Show this help");
    println!("    exit, quit, \\q      - Exit RustyDB");
    println!();
    println!("  Examples:");
    println!("    CREATE TABLE users (id INT PRIMARY KEY, name VARCHAR(100), age INT);");
    println!("    INSERT INTO users VALUES (1, 'Alice', 30);");
    println!("    SELECT * FROM users WHERE age > 25 ORDER BY name;");
    println!();
}

fn print_kv_help() {
    println!("\n  RustyDB Key-Value Mode");
    println!("  ======================\n");
    println!("  Basic Operations:");
    println!("    SET <key> <value>      - Store a key-value pair (sync, crash-safe)");
    println!("    SETASYNC <key> <value> - Store a key-value pair (async, faster)");
    println!("    GET <key>              - Retrieve a value by key");
    println!("    DELETE <key>           - Delete a key");
    println!("    LEN                    - Get number of keys");
    println!("    ISEMPTY                - Check if store is empty");
    println!("    CLEAR                  - Delete all keys");
    println!();
    println!("  Batch Operations:");
    println!("    MGET <key1> <key2> ... - Get multiple keys at once");
    println!("    MSET <k1> <v1> ...     - Set multiple key-value pairs");
    println!();
    println!("  Persistence:");
    println!("    SNAPSHOT               - Compact WAL into snapshot");
    println!("    FLUSH                  - Flush pending writes to disk");
    println!("    WALSIZE                - Show current WAL file size");
    println!();
    println!("  Other:");
    println!("    SQL                    - Info about SQL mode");
    println!("    HELP                   - Show this help message");
    println!("    EXIT                   - Quit RustyDB");
    println!();
}
