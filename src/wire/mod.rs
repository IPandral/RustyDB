pub mod protocol;

use sqlparser::dialect::MySqlDialect;
use sqlparser::tokenizer::{Token, Tokenizer};
use std::collections::HashMap;
use std::io::{self, ErrorKind};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsAcceptor;
use zeroize::Zeroizing;

use crate::server::{AppState, ServerConfig};
use crate::sql::{DataType, ExecutionResult, PreparedStatement, ResultSet, Row, SQLSession, Value};
use protocol::*;

static NEXT_CONNECTION_ID: AtomicU32 = AtomicU32::new(1);
const MAX_HANDSHAKE_PACKET: usize = 16 * 1024;
pub(crate) const MAX_PASSWORD_BYTES: usize = 1024;

trait WireStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> WireStream for T {}

struct WirePreparedStatement {
    prepared: PreparedStatement,
    parameter_types: Vec<u16>,
    long_data: HashMap<usize, Vec<u8>>,
}

async fn read_packet(stream: &mut (impl AsyncRead + Unpin)) -> io::Result<(u8, Vec<u8>)> {
    read_limited_packet(stream, 0xFF_FF_FF).await
}

async fn read_limited_packet(
    stream: &mut (impl AsyncRead + Unpin),
    limit: usize,
) -> io::Result<(u8, Vec<u8>)> {
    let mut header = [0u8; 4];
    stream.read_exact(&mut header).await?;

    let payload_len =
        (header[0] as usize) | ((header[1] as usize) << 8) | ((header[2] as usize) << 16);
    let seq_id = header[3];

    if payload_len > limit {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            "MySQL packet exceeds size limit",
        ));
    }
    let mut payload = vec![0u8; payload_len];
    stream.read_exact(&mut payload).await?;

    Ok((seq_id, payload))
}

async fn write_packet(
    stream: &mut (impl AsyncWrite + Unpin),
    seq_id: u8,
    payload: &[u8],
) -> io::Result<()> {
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
    stream: &mut (impl AsyncWrite + Unpin),
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

fn try_set_autocommit(query: &str, session: &SQLSession) -> Option<ExecutionResult> {
    let tokens = match Tokenizer::new(&MySqlDialect {}, query).tokenize() {
        Ok(tokens) => tokens,
        Err(error) => return Some(ExecutionResult::Error(error.to_string())),
    };
    let tokens: Vec<_> = tokens
        .into_iter()
        .filter(|token| !matches!(token, Token::Whitespace(_)))
        .collect();
    let [Token::Word(command), assignments @ ..] = tokens.as_slice() else {
        return None;
    };
    if !command.value.eq_ignore_ascii_case("SET") || command.quote_style.is_some() {
        return None;
    }

    let (global, value) =
        assignments
            .split(|token| *token == Token::Comma)
            .find_map(|assignment| {
                let operator = assignment.iter().position(|token| *token == Token::Eq);
                let (target, value) = assignment.split_at(operator.unwrap_or(assignment.len()));
                let target = target
                    .iter()
                    .map(|token| match token {
                        Token::Word(word) => word.value.clone(),
                        _ => token.to_string(),
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                let global = match target.to_ascii_lowercase().as_str() {
                    "autocommit"
                    | "session autocommit"
                    | "local autocommit"
                    | "@@autocommit"
                    | "@@session . autocommit"
                    | "@@local . autocommit" => false,
                    "global autocommit"
                    | "persist autocommit"
                    | "persist_only autocommit"
                    | "@@global . autocommit" => true,
                    _ => return None,
                };
                Some((global, value))
            })?;
    if global {
        return Some(ExecutionResult::Error(
            "Only session autocommit can be changed".to_string(),
        ));
    }
    let [Token::Eq, value] = value else {
        return Some(ExecutionResult::Error(
            "SET autocommit requires one assignment".to_string(),
        ));
    };
    if assignments.contains(&Token::Comma) {
        return Some(ExecutionResult::Error(
            "Multiple SET assignments involving autocommit are unsupported".to_string(),
        ));
    }
    let enabled = match value.to_string().to_ascii_lowercase().as_str() {
        "1" | "on" => true,
        "0" | "off" => false,
        _ => {
            return Some(ExecutionResult::Error(
                "SET autocommit requires ON, OFF, 1, or 0".to_string(),
            ));
        }
    };
    Some(session.set_autocommit(enabled))
}

fn try_handle_system_query(query: &str, session: &SQLSession) -> Option<ExecutionResult> {
    let trimmed = query.trim().trim_end_matches(';').trim();
    let upper = trimmed.to_uppercase();

    if upper.starts_with("SET")
        && upper.contains("AUTOCOMMIT")
        && let Some(result) = try_set_autocommit(trimmed, session)
    {
        return Some(result);
    }

    // SET commands
    if upper.starts_with("SET ") && !trimmed[4..].trim_start().starts_with('@') {
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
        return Some(handle_sysvar_query(trimmed, session.autocommit()));
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
fn handle_sysvar_query(query: &str, autocommit: bool) -> ExecutionResult {
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
        ("@@tx_isolation", "REPEATABLE-READ"),
        ("@@transaction_isolation", "REPEATABLE-READ"),
        ("@@lower_case_table_names", "1"),
        ("@@license", "Apache-2.0"),
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
    for (start, _) in query.match_indices("@@") {
        let name = &query[start + 2..];
        let end = name
            .find(|ch: char| !(ch.is_alphanumeric() || ch == '_' || ch == '.'))
            .unwrap_or(name.len());
        let var_name = &query[start..start + 2 + end];
        let var_lower = var_name.to_ascii_lowercase();
        let value = match var_lower.as_str() {
            "@@autocommit" | "@@session.autocommit" | "@@local.autocommit" => {
                Value::Integer(i64::from(autocommit))
            }
            "@@global.autocommit" => Value::Integer(1),
            _ => Value::Text(
                known_vars
                    .iter()
                    .find(|(name, _)| *name == var_lower)
                    .map_or_else(String::new, |(_, value)| value.to_string()),
            ),
        };
        columns.push(var_name.to_string());
        values.push(value);
    }

    if columns.is_empty() {
        columns.push("@@unknown".to_string());
        values.push(Value::Text(String::new()));
    }

    let mut rs = ResultSet::new(columns);
    rs.add_row(Row::new(values));
    ExecutionResult::Select(rs)
}

fn session_status(session: &SQLSession) -> u16 {
    let mut status = 0;
    if session.autocommit() {
        status |= SERVER_STATUS_AUTOCOMMIT;
    }
    if session.is_in_transaction() {
        status |= SERVER_STATUS_IN_TRANS;
    }
    status
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

fn encode_prepared_result(
    result: &ExecutionResult,
    result_metadata: &[(String, DataType)],
    status: u16,
) -> Vec<Vec<u8>> {
    match result {
        ExecutionResult::Select(result) => {
            let mut packets = Vec::new();
            packets.push(build_column_count(result.columns.len() as u64));
            for (index, column) in result.columns.iter().enumerate() {
                let data_type = result_metadata
                    .get(index)
                    .map(|(_, data_type)| data_type)
                    .unwrap_or(&DataType::Text);
                packets.push(build_column_def(
                    column,
                    "",
                    datatype_to_mysql(data_type),
                    column_flags(data_type, true, false),
                ));
            }
            packets.push(build_eof_packet_with_status(status));
            for row in &result.rows {
                packets.push(build_binary_row(&row.values));
            }
            packets.push(build_eof_packet_with_status(status));
            packets
        }
        _ => encode_result(result, status),
    }
}

fn decode_stmt_parameters(
    data: &[u8],
    statement: &mut WirePreparedStatement,
) -> Result<Vec<Value>, String> {
    let count = statement.prepared.parameter_count();
    if data.len() < 10 {
        return Err("COM_STMT_EXECUTE packet is too short".to_string());
    }
    let mut position = 10;
    let bitmap_len = count.div_ceil(8);
    if data.len().saturating_sub(position) < bitmap_len + 1 {
        return Err("Invalid parameter null bitmap".to_string());
    }
    let null_bitmap = &data[position..position + bitmap_len];
    position += bitmap_len;
    let new_types = data[position] != 0;
    position += 1;
    if new_types {
        if data.len().saturating_sub(position) < count * 2 {
            return Err("Missing prepared parameter types".to_string());
        }
        statement.parameter_types.clear();
        for _ in 0..count {
            statement
                .parameter_types
                .push(u16::from_le_bytes([data[position], data[position + 1]]));
            position += 2;
        }
    }
    if statement.parameter_types.len() != count {
        return Err("Parameter types were not supplied".to_string());
    }
    let mut values = Vec::with_capacity(count);
    for index in 0..count {
        if null_bitmap
            .get(index / 8)
            .is_some_and(|byte| byte & (1 << (index % 8)) != 0)
        {
            values.push(Value::Null);
            continue;
        }
        if let Some(long_data) = statement.long_data.remove(&index) {
            values.push(Value::Text(String::from_utf8(long_data).map_err(|_| {
                "Long parameter data is not valid UTF-8".to_string()
            })?));
            continue;
        }
        let type_code = statement.parameter_types[index] as u8;
        let unsigned = statement.parameter_types[index] & 0x8000 != 0;
        let take = |position: &mut usize, length: usize| -> Result<&[u8], String> {
            if data.len().saturating_sub(*position) < length {
                return Err("Truncated prepared parameter".to_string());
            }
            let value = &data[*position..*position + length];
            *position += length;
            Ok(value)
        };
        let value = match type_code {
            MYSQL_TYPE_NULL => Value::Null,
            MYSQL_TYPE_TINY => {
                let raw = take(&mut position, 1)?[0];
                Value::Integer(if unsigned {
                    i64::from(raw)
                } else {
                    i64::from(raw as i8)
                })
            }
            MYSQL_TYPE_SHORT => {
                let raw = take(&mut position, 2)?;
                let raw = u16::from_le_bytes([raw[0], raw[1]]);
                Value::Integer(if unsigned {
                    i64::from(raw)
                } else {
                    i64::from(raw as i16)
                })
            }
            MYSQL_TYPE_LONG => {
                let raw = take(&mut position, 4)?;
                let raw = u32::from_le_bytes(raw.try_into().unwrap());
                Value::Integer(if unsigned {
                    i64::from(raw)
                } else {
                    i64::from(raw as i32)
                })
            }
            MYSQL_TYPE_LONGLONG => {
                let raw = take(&mut position, 8)?;
                let raw = u64::from_le_bytes(raw.try_into().unwrap());
                if unsigned && raw > i64::MAX as u64 {
                    return Err("Unsigned integer exceeds RustyDB INTEGER range".to_string());
                }
                Value::Integer(raw as i64)
            }
            MYSQL_TYPE_FLOAT => {
                let raw = take(&mut position, 4)?;
                Value::Float(f32::from_le_bytes(raw.try_into().unwrap()) as f64)
            }
            MYSQL_TYPE_DOUBLE => {
                let raw = take(&mut position, 8)?;
                Value::Float(f64::from_le_bytes(raw.try_into().unwrap()))
            }
            MYSQL_TYPE_VAR_STRING | 0x0F | 0xFC | 0xFE => {
                let length = read_lenenc_int(data, &mut position)
                    .ok_or_else(|| "Invalid string parameter length".to_string())?
                    as usize;
                let raw = take(&mut position, length)?;
                Value::Text(
                    String::from_utf8(raw.to_vec())
                        .map_err(|_| "String parameter is not valid UTF-8".to_string())?,
                )
            }
            other => return Err(format!("Unsupported prepared parameter type 0x{other:02X}")),
        };
        values.push(value);
    }
    Ok(values)
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

async fn complete_handshake(
    mut stream: TcpStream,
    config: &ServerConfig,
    tls: Option<TlsAcceptor>,
    conn_id: u32,
) -> io::Result<Box<dyn WireStream>> {
    let scramble = generate_scramble().map_err(|error| io::Error::other(error.to_string()))?;
    write_packet(
        &mut stream,
        0,
        &build_handshake_v10(conn_id, &scramble, tls.is_some()),
    )
    .await?;
    let (sequence, packet) = read_limited_packet(&mut stream, MAX_HANDSHAKE_PACKET).await?;
    if sequence != 1 {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            "Unexpected handshake packet sequence",
        ));
    }

    let Some(acceptor) = tls else {
        let handshake = parse_client_handshake(&packet).map_err(io::Error::other)?;
        if handshake.capabilities & (CLIENT_SSL | CLIENT_PROTOCOL_41) != CLIENT_PROTOCOL_41 {
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                "Unsupported local handshake",
            ));
        }
        write_packet(&mut stream, 2, &build_ok_packet(0, 0)).await?;
        return Ok(Box::new(stream));
    };

    // Only the fixed-size SSLRequest is accepted before TLS on an authenticated listener.
    let ssl_flags = CLIENT_SSL | CLIENT_PROTOCOL_41;
    if packet.len() != 32
        || u32::from_le_bytes(packet[..4].try_into().unwrap()) & ssl_flags != ssl_flags
    {
        let error = build_err_packet(3159, "HY000", "TLS is required for authentication");
        write_packet(&mut stream, 2, &error).await?;
        return Err(io::Error::new(ErrorKind::PermissionDenied, "TLS required"));
    }
    let mut stream = acceptor.accept(stream).await?;
    let (sequence, packet) = read_limited_packet(&mut stream, MAX_HANDSHAKE_PACKET).await?;
    let handshake = parse_client_handshake(&packet).map_err(io::Error::other)?;
    if sequence != 2
        || handshake.capabilities & ssl_flags != ssl_flags
        || handshake.auth_plugin.as_deref() != Some(AUTH_PLUGIN)
    {
        let error = build_err_packet(1045, "28000", "Access denied");
        write_packet(&mut stream, 3, &error).await?;
        return Err(io::Error::new(
            ErrorKind::PermissionDenied,
            "Unsupported authentication handshake",
        ));
    }

    // Use caching_sha2_password's full authentication exchange after establishing TLS.
    // RustyDB does not maintain a fast-authentication password hash cache.
    write_packet(&mut stream, 3, &[0x01, 0x04]).await?;
    let (sequence, response) = read_limited_packet(&mut stream, MAX_PASSWORD_BYTES + 1).await?;
    let response = Zeroizing::new(response);
    let authenticated = sequence == 4
        && response.strip_suffix(&[0]).is_some_and(|password| {
            config.credentials_match(handshake.username.as_bytes(), password)
        });
    if !authenticated {
        let error = build_err_packet(1045, "28000", "Access denied");
        write_packet(&mut stream, 5, &error).await?;
        return Err(io::Error::new(ErrorKind::PermissionDenied, "Access denied"));
    }
    write_packet(&mut stream, 5, &build_ok_packet(0, 0)).await?;
    Ok(Box::new(stream))
}

async fn handle_commands(state: Arc<AppState>, mut stream: Box<dyn WireStream>, conn_id: u32) {
    eprintln!("[wire] Handshake completed (connection {conn_id})");

    // Each wire connection owns a transaction/session context.
    let session = state.sql_db.session();
    let mut prepared_statements: HashMap<u32, WirePreparedStatement> = HashMap::new();
    let mut next_statement_id = 1u32;

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
                let ok = build_ok_packet_with_status(0, 0, session_status(&session));
                if write_packet(&mut stream, 1, &ok).await.is_err() {
                    break;
                }
            }

            COM_INIT_DB => {
                // Accept any database name
                let ok = build_ok_packet_with_status(0, 0, session_status(&session));
                if write_packet(&mut stream, 1, &ok).await.is_err() {
                    break;
                }
            }

            COM_FIELD_LIST => {
                // Deprecated command - return EOF
                let eof = build_eof_packet_with_status(session_status(&session));
                if write_packet(&mut stream, 1, &eof).await.is_err() {
                    break;
                }
            }

            COM_QUERY => {
                let query = String::from_utf8_lossy(&data[1..]).to_string();

                // Try system query handler first, then fall through to SQL engine
                let result = if let Some(sys_result) = try_handle_system_query(&query, &session) {
                    sys_result
                } else {
                    session.execute(&query)
                };

                let packets = encode_result(&result, session_status(&session));
                if write_packets(&mut stream, 1, &packets).await.is_err() {
                    break;
                }
            }

            COM_STMT_PREPARE => {
                let sql = String::from_utf8_lossy(&data[1..]).to_string();
                match session.prepare(&sql) {
                    Ok(prepared) => {
                        let statement_id = next_statement_id;
                        next_statement_id = next_statement_id.wrapping_add(1).max(1);
                        let count = prepared.parameter_count();
                        let result_metadata = session.prepared_result_metadata(&prepared, None);
                        let status = session_status(&session);
                        let mut packets = vec![build_stmt_prepare_ok(
                            statement_id,
                            result_metadata.len() as u16,
                            count as u16,
                        )];
                        for index in 0..count {
                            packets.push(build_column_def(
                                &format!("param{}", index + 1),
                                "",
                                MYSQL_TYPE_VAR_STRING,
                                0,
                            ));
                        }
                        if count > 0 {
                            packets.push(build_eof_packet_with_status(status));
                        }
                        for (name, data_type) in &result_metadata {
                            packets.push(build_column_def(
                                name,
                                "",
                                datatype_to_mysql(data_type),
                                column_flags(data_type, true, false),
                            ));
                        }
                        if !result_metadata.is_empty() {
                            packets.push(build_eof_packet_with_status(status));
                        }
                        prepared_statements.insert(
                            statement_id,
                            WirePreparedStatement {
                                prepared,
                                parameter_types: Vec::new(),
                                long_data: HashMap::new(),
                            },
                        );
                        if write_packets(&mut stream, 1, &packets).await.is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        let packet = build_err_packet(1064, "42000", &error);
                        if write_packet(&mut stream, 1, &packet).await.is_err() {
                            break;
                        }
                    }
                }
            }

            COM_STMT_EXECUTE => {
                if data.len() < 10 {
                    let packet =
                        build_err_packet(1243, "HY000", "Invalid statement execute packet");
                    if write_packet(&mut stream, 1, &packet).await.is_err() {
                        break;
                    }
                    continue;
                }
                let statement_id = u32::from_le_bytes(data[1..5].try_into().unwrap());
                let Some(statement) = prepared_statements.get_mut(&statement_id) else {
                    let packet =
                        build_err_packet(1243, "HY000", "Unknown prepared statement handler");
                    if write_packet(&mut stream, 1, &packet).await.is_err() {
                        break;
                    }
                    continue;
                };
                let parameters = match decode_stmt_parameters(&data, statement) {
                    Ok(parameters) => parameters,
                    Err(error) => {
                        let packet = build_err_packet(1210, "HY000", &error);
                        if write_packet(&mut stream, 1, &packet).await.is_err() {
                            break;
                        }
                        continue;
                    }
                };
                let result_metadata =
                    session.prepared_result_metadata(&statement.prepared, Some(&parameters));
                let result = session.execute_prepared(&statement.prepared, &parameters);
                let packets =
                    encode_prepared_result(&result, &result_metadata, session_status(&session));
                if write_packets(&mut stream, 1, &packets).await.is_err() {
                    break;
                }
            }

            COM_STMT_SEND_LONG_DATA => {
                if data.len() < 7 {
                    // This command has no response packet. Ignore malformed payloads without
                    // dropping an otherwise healthy connection.
                    continue;
                }
                let statement_id = u32::from_le_bytes(data[1..5].try_into().unwrap());
                let parameter_id = u16::from_le_bytes(data[5..7].try_into().unwrap()) as usize;
                if let Some(statement) = prepared_statements.get_mut(&statement_id) {
                    statement
                        .long_data
                        .entry(parameter_id)
                        .or_default()
                        .extend_from_slice(&data[7..]);
                }
                // COM_STMT_SEND_LONG_DATA has no response packet.
            }

            COM_STMT_RESET => {
                if data.len() < 5 {
                    let packet = build_err_packet(1243, "HY000", "Invalid statement reset packet");
                    if write_packet(&mut stream, 1, &packet).await.is_err() {
                        break;
                    }
                    continue;
                }
                let statement_id = u32::from_le_bytes(data[1..5].try_into().unwrap());
                let packet = if let Some(statement) = prepared_statements.get_mut(&statement_id) {
                    statement.long_data.clear();
                    build_ok_packet_with_status(0, 0, session_status(&session))
                } else {
                    build_err_packet(1243, "HY000", "Unknown prepared statement handler")
                };
                if write_packet(&mut stream, 1, &packet).await.is_err() {
                    break;
                }
            }

            COM_STMT_CLOSE => {
                if data.len() >= 5 {
                    let statement_id = u32::from_le_bytes(data[1..5].try_into().unwrap());
                    prepared_statements.remove(&statement_id);
                }
                // COM_STMT_CLOSE has no response packet.
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
    let tls = state.config.tls_acceptor()?;
    let listener = TcpListener::bind((state.config.host.as_str(), port)).await?;
    serve_wire(listener, state, tls).await?;
    Ok(())
}

pub(crate) async fn serve_wire(
    listener: TcpListener,
    state: Arc<AppState>,
    tls: Option<TlsAcceptor>,
) -> io::Result<()> {
    eprintln!(
        "[wire] MySQL wire protocol listening on {}",
        listener.local_addr()?
    );
    let handshake_slots = Arc::new(tokio::sync::Semaphore::new(128));

    loop {
        let (stream, _) = listener.accept().await?;
        let Ok(slot) = Arc::clone(&handshake_slots).try_acquire_owned() else {
            continue;
        };
        let _ = stream.set_nodelay(true);
        let state = Arc::clone(&state);
        let tls = tls.clone();
        let conn_id = NEXT_CONNECTION_ID.fetch_add(1, Ordering::Relaxed);

        tokio::spawn(async move {
            let handshake = tokio::time::timeout(
                Duration::from_secs(10),
                complete_handshake(stream, &state.config, tls, conn_id),
            )
            .await;
            drop(slot);
            match handshake {
                Ok(Ok(stream)) => handle_commands(state, stream, conn_id).await,
                _ => eprintln!("[wire] Handshake failed (connection {conn_id})"),
            }
        });
    }
}
