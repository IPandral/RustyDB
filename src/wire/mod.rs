pub mod protocol;

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::server::AppState;
use crate::sql::{ExecutionResult, ResultSet, Row, Value};
use protocol::*;

static NEXT_CONNECTION_ID: AtomicU32 = AtomicU32::new(1);

async fn read_packet(stream: &mut TcpStream) -> std::io::Result<(u8, Vec<u8>)> {
    let mut header = [0u8; 4];
    stream.read_exact(&mut header).await?;

    let payload_len =
        (header[0] as usize) | ((header[1] as usize) << 8) | ((header[2] as usize) << 16);
    let seq_id = header[3];

    let mut payload = vec![0u8; payload_len];
    stream.read_exact(&mut payload).await?;

    Ok((seq_id, payload))
}

async fn write_packet(stream: &mut TcpStream, seq_id: u8, payload: &[u8]) -> std::io::Result<()> {
    let len = payload.len();
    let header = [
        (len & 0xFF) as u8,
        ((len >> 8) & 0xFF) as u8,
        ((len >> 16) & 0xFF) as u8,
        seq_id,
    ];
    stream.write_all(&header).await?;
    stream.write_all(payload).await?;
    stream.flush().await?;
    Ok(())
}

/// Write multiple payloads as sequential packets with incrementing sequence IDs.
/// Returns the next sequence ID after the last packet sent.
async fn write_packets(
    stream: &mut TcpStream,
    start_seq: u8,
    payloads: &[Vec<u8>],
) -> std::io::Result<u8> {
    let mut seq = start_seq;
    for payload in payloads {
        write_packet(stream, seq, payload).await?;
        seq = seq.wrapping_add(1);
    }
    Ok(seq)
}

fn try_handle_system_query(query: &str) -> Option<ExecutionResult> {
    let trimmed = query.trim().trim_end_matches(';').trim();
    let upper = trimmed.to_uppercase();

    // SET commands
    if upper.starts_with("SET ") {
        return Some(ExecutionResult::RowsAffected(0));
    }

    // USE database
    if upper.starts_with("USE ") {
        return Some(ExecutionResult::RowsAffected(0));
    }

    // KILL
    if upper.starts_with("KILL") {
        return Some(ExecutionResult::RowsAffected(0));
    }

    // Queries containing system variables (@@...)
    if upper.starts_with("SELECT") && upper.contains("@@") {
        return Some(handle_sysvar_query(trimmed));
    }

    // SELECT DATABASE()
    if upper.starts_with("SELECT") && upper.contains("DATABASE()") {
        let mut rs = ResultSet::new(vec!["DATABASE()".to_string()]);
        rs.add_row(Row::new(vec![Value::Text("rustydb".to_string())]));
        return Some(ExecutionResult::Select(rs));
    }

    // SELECT USER() / CURRENT_USER()
    if upper.starts_with("SELECT") && (upper.contains("USER()") || upper.contains("CURRENT_USER")) {
        let mut rs = ResultSet::new(vec!["USER()".to_string()]);
        rs.add_row(Row::new(vec![Value::Text(
            "rustydb_user@localhost".to_string(),
        )]));
        return Some(ExecutionResult::Select(rs));
    }

    // SELECT 1 (health check)
    if upper == "SELECT 1" {
        let mut rs = ResultSet::new(vec!["1".to_string()]);
        rs.add_row(Row::new(vec![Value::Integer(1)]));
        return Some(ExecutionResult::Select(rs));
    }

    // SHOW WARNINGS
    if upper.starts_with("SHOW WARNINGS") {
        let rs = ResultSet::new(vec![
            "Level".to_string(),
            "Code".to_string(),
            "Message".to_string(),
        ]);
        return Some(ExecutionResult::Select(rs));
    }

    // SHOW DATABASES
    if upper == "SHOW DATABASES" {
        let mut rs = ResultSet::new(vec!["Database".to_string()]);
        rs.add_row(Row::new(vec![Value::Text("rustydb".to_string())]));
        return Some(ExecutionResult::Select(rs));
    }

    // SHOW VARIABLES, SHOW STATUS, SHOW COLLATION, etc. (not SHOW TABLES - let SQL engine handle)
    if upper.starts_with("SHOW ")
        && !upper.starts_with("SHOW TABLES")
        && !upper.starts_with("SHOW CREATE")
    {
        let rs = ResultSet::new(vec!["Variable_name".to_string(), "Value".to_string()]);
        return Some(ExecutionResult::Select(rs));
    }

    None
}

/// Handle SELECT queries containing @@system_variables.
fn handle_sysvar_query(query: &str) -> ExecutionResult {
    let known_vars: &[(&str, &str)] = &[
        ("@@version_comment", "RustyDB SQL Engine"),
        ("@@version", SERVER_VERSION),
        ("@@max_allowed_packet", "67108864"),
        ("@@character_set_client", "utf8mb4"),
        ("@@character_set_connection", "utf8mb4"),
        ("@@character_set_results", "utf8mb4"),
        ("@@character_set_server", "utf8mb4"),
        ("@@character_set_database", "utf8mb4"),
        ("@@collation_connection", "utf8mb4_general_ci"),
        ("@@collation_server", "utf8mb4_general_ci"),
        ("@@collation_database", "utf8mb4_general_ci"),
        ("@@interactive_timeout", "28800"),
        ("@@wait_timeout", "28800"),
        ("@@net_write_timeout", "60"),
        ("@@sql_mode", "TRADITIONAL"),
        ("@@time_zone", "SYSTEM"),
        ("@@system_time_zone", "UTC"),
        ("@@autocommit", "1"),
        ("@@tx_isolation", "REPEATABLE-READ"),
        ("@@transaction_isolation", "REPEATABLE-READ"),
        ("@@lower_case_table_names", "1"),
        ("@@license", "Apache-2.0"),
        ("@@session.autocommit", "1"),
        ("@@session.transaction_read_only", "0"),
        ("@@session.auto_increment_increment", "1"),
        ("@@session.transaction_isolation", "REPEATABLE-READ"),
        ("@@session.tx_isolation", "REPEATABLE-READ"),
        ("@@session.sql_mode", "TRADITIONAL"),
        ("@@global.max_allowed_packet", "67108864"),
        ("@@global.net_buffer_length", "16384"),
    ];

    let mut columns = Vec::new();
    let mut values = Vec::new();
    let chars: Vec<char> = query.chars().collect();
    let mut i = 0;

    while i < chars.len().saturating_sub(1) {
        if chars[i] == '@' && chars[i + 1] == '@' {
            let start = i;
            i += 2;
            while i < chars.len()
                && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '.')
            {
                i += 1;
            }
            let var_name = &query[start..i];
            let var_lower = var_name.to_lowercase();

            let value = known_vars
                .iter()
                .find(|(k, _)| k.to_lowercase() == var_lower)
                .map(|(_, v)| v.to_string())
                .unwrap_or_default();

            columns.push(var_name.to_string());
            values.push(Value::Text(value));
        } else {
            i += 1;
        }
    }

    if columns.is_empty() {
        columns.push("@@unknown".to_string());
        values.push(Value::Text(String::new()));
    }

    let mut rs = ResultSet::new(columns);
    rs.add_row(Row::new(values));
    ExecutionResult::Select(rs)
}

fn encode_result(result: &ExecutionResult, status: u16) -> Vec<Vec<u8>> {
    match result {
        ExecutionResult::Select(rs) => encode_result_set(rs, "", status),
        ExecutionResult::RowsAffected(n) => vec![build_ok_packet_with_status(*n as u64, 0, status)],
        ExecutionResult::TableCreated(_name) => {
            vec![build_ok_packet_with_status(0, 0, status)]
        }
        ExecutionResult::TableDropped(_name) => {
            vec![build_ok_packet_with_status(0, 0, status)]
        }
        ExecutionResult::IndexCreated(_)
        | ExecutionResult::IndexDropped(_)
        | ExecutionResult::TransactionStarted
        | ExecutionResult::TransactionCommitted
        | ExecutionResult::TransactionRolledBack => {
            vec![build_ok_packet_with_status(0, 0, status)]
        }
        ExecutionResult::Tables(tables) => {
            let mut rs = ResultSet::new(vec!["Tables_in_rustydb".to_string()]);
            for t in tables {
                rs.add_row(Row::new(vec![Value::Text(t.clone())]));
            }
            encode_result_set(&rs, "", status)
        }
        ExecutionResult::TableDescription {
            table_name,
            columns,
        } => {
            let mut rs = ResultSet::new(vec![
                "Field".to_string(),
                "Type".to_string(),
                "Null".to_string(),
                "Key".to_string(),
                "Default".to_string(),
                "Extra".to_string(),
            ]);
            for col in columns {
                rs.add_row(Row::new(vec![
                    Value::Text(col.name.clone()),
                    Value::Text(col.data_type.to_string()),
                    Value::Text(if col.nullable { "YES" } else { "NO" }.to_string()),
                    Value::Text(if col.primary_key { "PRI" } else { "" }.to_string()),
                    Value::Text("NULL".to_string()),
                    Value::Text(String::new()),
                ]));
            }
            encode_result_set(&rs, table_name, status)
        }
        ExecutionResult::Indexes(indexes) => {
            let mut rs = ResultSet::new(vec![
                "Key_name".to_string(),
                "Column_name".to_string(),
                "Non_unique".to_string(),
            ]);
            for index in indexes {
                for column in &index.columns {
                    rs.add_row(Row::new(vec![
                        Value::Text(index.name.clone()),
                        Value::Text(column.clone()),
                        Value::Integer(if index.unique { 0 } else { 1 }),
                    ]));
                }
            }
            encode_result_set(&rs, "", status)
        }
        ExecutionResult::Explain(lines) => {
            let mut rs = ResultSet::new(vec!["plan".to_string()]);
            for line in lines {
                rs.add_row(Row::new(vec![Value::Text(line.clone())]));
            }
            encode_result_set(&rs, "", status)
        }
        ExecutionResult::Error(msg) => {
            vec![build_err_packet(1064, "42000", msg)]
        }
    }
}

/// Encode a ResultSet as MySQL text protocol packets.
fn encode_result_set(rs: &ResultSet, table_name: &str, status: u16) -> Vec<Vec<u8>> {
    let mut packets = Vec::new();

    // Column count
    packets.push(build_column_count(rs.columns.len() as u64));

    // Column definitions
    for col_name in &rs.columns {
        // Infer column type from first row's data if available
        let (col_type, col_flags) = if !rs.rows.is_empty() {
            let col_idx = rs.columns.iter().position(|c| c == col_name).unwrap_or(0);
            if let Some(value) = rs.rows[0].values.get(col_idx) {
                let dt = value.data_type();
                (datatype_to_mysql(&dt), column_flags(&dt, true, false))
            } else {
                (MYSQL_TYPE_VAR_STRING, 0)
            }
        } else {
            (MYSQL_TYPE_VAR_STRING, 0)
        };

        packets.push(build_column_def(col_name, table_name, col_type, col_flags));
    }

    // EOF after column definitions
    packets.push(build_eof_packet_with_status(status));

    // Row data
    for row in &rs.rows {
        let value_refs: Vec<&Value> = row.values.iter().collect();
        packets.push(build_text_row(&value_refs));
    }

    // EOF after rows
    packets.push(build_eof_packet_with_status(status));

    packets
}

async fn handle_connection(state: Arc<AppState>, mut stream: TcpStream, conn_id: u32) {
    // Disable Nagle for lower latency
    let _ = stream.set_nodelay(true);

    let peer = stream
        .peer_addr()
        .map(|a| a.to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    eprintln!("[wire] Connection {} from {}", conn_id, peer);

    // === HANDSHAKE PHASE ===

    // Generate scramble and send server greeting
    let scramble = generate_scramble(conn_id);
    let greeting = build_handshake_v10(conn_id, &scramble);
    if write_packet(&mut stream, 0, &greeting).await.is_err() {
        return;
    }

    // Read client handshake response
    let (_seq, client_data) = match read_packet(&mut stream).await {
        Ok(p) => p,
        Err(_) => return,
    };

    let handshake = match parse_client_handshake(&client_data) {
        Ok(h) => h,
        Err(e) => {
            let err = build_err_packet(1045, "28000", &format!("Bad handshake: {}", e));
            let _ = write_packet(&mut stream, 2, &err).await;
            return;
        }
    };

    // Authenticate
    let auth_ok = if state.config.auth_required() {
        let expected_user = state.config.username.as_ref().unwrap();
        let expected_pass = state.config.password.as_ref().unwrap();

        handshake.username == *expected_user
            && verify_mysql_native_password(expected_pass, &scramble, &handshake.auth_response)
    } else {
        true // No auth configured, accept all connections
    };

    if !auth_ok {
        let err = build_err_packet(
            1045,
            "28000",
            &format!("Access denied for user '{}'@'{}'", handshake.username, peer),
        );
        let _ = write_packet(&mut stream, 2, &err).await;
        eprintln!(
            "[wire] Auth failed for user '{}' from {}",
            handshake.username, peer
        );
        return;
    }

    // Send OK to complete handshake
    let ok = build_ok_packet(0, 0);
    if write_packet(&mut stream, 2, &ok).await.is_err() {
        return;
    }

    eprintln!(
        "[wire] User '{}' authenticated (conn {})",
        handshake.username, conn_id
    );

    // Each wire connection owns a transaction/session context.
    let session = state.sql_db.session();

    // === COMMAND PHASE ===
    loop {
        let (_seq, data) = match read_packet(&mut stream).await {
            Ok(p) => p,
            Err(_) => break, // Connection closed
        };

        if data.is_empty() {
            break;
        }

        let cmd = data[0];
        match cmd {
            COM_QUIT => {
                break;
            }

            COM_PING => {
                let ok = build_ok_packet(0, 0);
                if write_packet(&mut stream, 1, &ok).await.is_err() {
                    break;
                }
            }

            COM_INIT_DB => {
                // Accept any database name
                let ok = build_ok_packet(0, 0);
                if write_packet(&mut stream, 1, &ok).await.is_err() {
                    break;
                }
            }

            COM_FIELD_LIST => {
                // Deprecated command - return EOF
                let eof = build_eof_packet();
                if write_packet(&mut stream, 1, &eof).await.is_err() {
                    break;
                }
            }

            COM_QUERY => {
                let query = String::from_utf8_lossy(&data[1..]).to_string();

                // Try system query handler first, then fall through to SQL engine
                let result = if let Some(sys_result) = try_handle_system_query(&query) {
                    sys_result
                } else {
                    session.execute(&query)
                };

                let status = if session.is_in_transaction() {
                    SERVER_STATUS_IN_TRANS
                } else {
                    SERVER_STATUS_AUTOCOMMIT
                };
                let packets = encode_result(&result, status);
                if write_packets(&mut stream, 1, &packets).await.is_err() {
                    break;
                }
            }

            other => {
                let err = build_err_packet(
                    1047,
                    "08S01",
                    &format!("Unsupported command: 0x{:02X}", other),
                );
                if write_packet(&mut stream, 1, &err).await.is_err() {
                    break;
                }
            }
        }
    }

    eprintln!("[wire] Connection {} closed", conn_id);
}

pub async fn start_wire_server(
    state: Arc<AppState>,
    port: u16,
) -> Result<(), Box<dyn std::error::Error>> {
    let addr = format!("{}:{}", state.config.host, port);
    let listener = TcpListener::bind(&addr).await?;

    eprintln!("[wire] MySQL wire protocol listening on {}", addr);

    loop {
        let (stream, _addr) = listener.accept().await?;
        let state = Arc::clone(&state);
        let conn_id = NEXT_CONNECTION_ID.fetch_add(1, Ordering::Relaxed);

        tokio::spawn(async move {
            handle_connection(state, stream, conn_id).await;
        });
    }
}
