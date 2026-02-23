//! Integration tests for the MySQL wire protocol implementation.
//!
//! Tests cover:
//! - Protocol packet building (handshake, OK, ERR, EOF, column def, text row)
//! - Length-encoded integer and string encoding
//! - Scramble generation and mysql_native_password authentication
//! - Client handshake response parsing
//! - Full TCP integration: connect, handshake, query, ping, quit

#![cfg(feature = "server")]

use std::sync::Arc;

use rustydb::wire::protocol::*;
use rustydb::{AppState, KVStore, SQLDatabase, ServerConfig, Value};

fn test_config(port: u16) -> ServerConfig {
    ServerConfig {
        host: "127.0.0.1".to_string(),
        port: 0,       // HTTP port - unused in wire tests
        username: None, // no auth
        password: None,
        data_dir: String::new(),
        memory_only: true,
        wire_port: port,
    }
}

fn test_config_with_auth(port: u16, user: &str, pass: &str) -> ServerConfig {
    ServerConfig {
        host: "127.0.0.1".to_string(),
        port: 0,
        username: Some(user.to_string()),
        password: Some(pass.to_string()),
        data_dir: String::new(),
        memory_only: true,
        wire_port: port,
    }
}

fn test_app_state(cfg: ServerConfig) -> Arc<AppState> {
    Arc::new(AppState {
        kv_store: KVStore::new(),
        sql_db: SQLDatabase::new(),
        config: cfg,
    })
}

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

async fn read_packet(stream: &mut TcpStream) -> (u8, Vec<u8>) {
    let mut header = [0u8; 4];
    stream.read_exact(&mut header).await.expect("read header");

    let payload_len =
        (header[0] as usize) | ((header[1] as usize) << 8) | ((header[2] as usize) << 16);
    let seq_id = header[3];

    let mut payload = vec![0u8; payload_len];
    stream.read_exact(&mut payload).await.expect("read payload");

    (seq_id, payload)
}

async fn write_packet(stream: &mut TcpStream, seq_id: u8, payload: &[u8]) {
    let len = payload.len();
    let header = [
        (len & 0xFF) as u8,
        ((len >> 8) & 0xFF) as u8,
        ((len >> 16) & 0xFF) as u8,
        seq_id,
    ];
    stream.write_all(&header).await.expect("write header");
    stream.write_all(payload).await.expect("write payload");
    stream.flush().await.expect("flush");
}

fn build_client_handshake_response(
    username: &str,
    auth_response: &[u8],
    database: Option<&str>,
) -> Vec<u8> {
    let mut caps: u32 = CLIENT_PROTOCOL_41
        | CLIENT_SECURE_CONNECTION
        | CLIENT_LONG_PASSWORD
        | CLIENT_LONG_FLAG
        | CLIENT_TRANSACTIONS
        | CLIENT_PLUGIN_AUTH;

    if database.is_some() {
        caps |= CLIENT_CONNECT_WITH_DB;
    }

    let mut pkt = Vec::with_capacity(128);

    // Capability flags (4 bytes LE)
    pkt.extend_from_slice(&caps.to_le_bytes());

    // Max packet size (4 bytes)
    pkt.extend_from_slice(&(16_777_216u32).to_le_bytes());

    // Character set (1 byte) - utf8mb4
    pkt.push(CHARSET_UTF8MB4);

    // Reserved (23 zero bytes)
    pkt.extend_from_slice(&[0u8; 23]);

    // Username (null-terminated)
    pkt.extend_from_slice(username.as_bytes());
    pkt.push(0);

    // Auth response length + data (CLIENT_SECURE_CONNECTION style)
    pkt.push(auth_response.len() as u8);
    pkt.extend_from_slice(auth_response);

    // Database (null-terminated, if requested)
    if let Some(db) = database {
        pkt.extend_from_slice(db.as_bytes());
        pkt.push(0);
    }

    // Auth plugin name (null-terminated)
    pkt.extend_from_slice(b"mysql_native_password");
    pkt.push(0);

    pkt
}

fn compute_auth_token(password: &str, scramble: &[u8]) -> Vec<u8> {
    use sha1::{Digest, Sha1};

    if password.is_empty() {
        return Vec::new();
    }

    let stage1: [u8; 20] = Sha1::digest(password.as_bytes()).into();
    let stage2: [u8; 20] = Sha1::digest(stage1).into();

    let mut concat = Vec::with_capacity(scramble.len() + 20);
    concat.extend_from_slice(scramble);
    concat.extend_from_slice(&stage2);
    let scramble_hash: [u8; 20] = Sha1::digest(&concat).into();

    let mut token = [0u8; 20];
    for i in 0..20 {
        token[i] = stage1[i] ^ scramble_hash[i];
    }
    token.to_vec()
}

fn extract_scramble_from_greeting(payload: &[u8]) -> [u8; 20] {
    // Skip: protocol_version (1) + server_version (null-terminated)
    let mut pos = 1;
    while pos < payload.len() && payload[pos] != 0 {
        pos += 1;
    }
    pos += 1; // skip null terminator

    // Connection ID (4 bytes)
    pos += 4;

    // Auth plugin data part 1 (8 bytes)
    let mut scramble = [0u8; 20];
    scramble[..8].copy_from_slice(&payload[pos..pos + 8]);
    pos += 8;

    // Filler (1) + capabilities_lo (2) + charset (1) + status (2) +
    // capabilities_hi (2) + auth_data_len (1) + reserved (10) = 20
    pos += 1 + 2 + 1 + 2 + 2 + 1 + 10;

    // Auth plugin data part 2 (12 bytes + NUL)
    scramble[8..20].copy_from_slice(&payload[pos..pos + 12]);

    scramble
}

async fn random_port() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind to random port");
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

#[test]
fn test_encode_lenenc_int_single_byte() {
    assert_eq!(encode_lenenc_int(0), vec![0x00]);
    assert_eq!(encode_lenenc_int(1), vec![0x01]);
    assert_eq!(encode_lenenc_int(250), vec![250]);
}

#[test]
fn test_encode_lenenc_int_two_byte() {
    let encoded = encode_lenenc_int(251);
    assert_eq!(encoded[0], 0xFC);
    let val = u16::from_le_bytes([encoded[1], encoded[2]]);
    assert_eq!(val, 251);

    let encoded = encode_lenenc_int(65535);
    assert_eq!(encoded[0], 0xFC);
    let val = u16::from_le_bytes([encoded[1], encoded[2]]);
    assert_eq!(val, 65535);
}

#[test]
fn test_encode_lenenc_int_three_byte() {
    let encoded = encode_lenenc_int(65536);
    assert_eq!(encoded[0], 0xFD);
    let val = (encoded[1] as u64) | ((encoded[2] as u64) << 8) | ((encoded[3] as u64) << 16);
    assert_eq!(val, 65536);
}

#[test]
fn test_encode_lenenc_int_eight_byte() {
    let big: u64 = 16_777_216;
    let encoded = encode_lenenc_int(big);
    assert_eq!(encoded[0], 0xFE);
    let val = u64::from_le_bytes([
        encoded[1], encoded[2], encoded[3], encoded[4], encoded[5], encoded[6], encoded[7],
        encoded[8],
    ]);
    assert_eq!(val, big);
}

#[test]
fn test_encode_lenenc_str() {
    let encoded = encode_lenenc_str("hello");
    // Length prefix (1 byte for len < 251) + 5 bytes payload
    assert_eq!(encoded[0], 5);
    assert_eq!(&encoded[1..], b"hello");
}

#[test]
fn test_encode_lenenc_str_empty() {
    let encoded = encode_lenenc_str("");
    assert_eq!(encoded, vec![0]);
}

#[test]
fn test_read_null_terminated_basic() {
    let data = b"hello\x00world\x00";
    let mut pos = 0;
    let s = read_null_terminated(data, &mut pos);
    assert_eq!(s, "hello");
    assert_eq!(pos, 6); // past the null byte

    let s2 = read_null_terminated(data, &mut pos);
    assert_eq!(s2, "world");
}

#[test]
fn test_read_null_terminated_at_end() {
    // No null terminator - reads to end
    let data = b"abc";
    let mut pos = 0;
    let s = read_null_terminated(data, &mut pos);
    assert_eq!(s, "abc");
    assert_eq!(pos, 3);
}

#[test]
fn test_generate_scramble_no_zeros() {
    for conn_id in 0..100 {
        let scramble = generate_scramble(conn_id);
        assert_eq!(scramble.len(), 20);
        for b in &scramble {
            assert_ne!(*b, 0, "scramble must not contain zero bytes");
        }
    }
}

#[test]
fn test_generate_scramble_varies_by_conn_id() {
    let s1 = generate_scramble(1);
    let s2 = generate_scramble(2);
    // Extremely unlikely to collide; this is a basic sanity check.
    assert_ne!(s1, s2);
}

#[test]
fn test_verify_mysql_native_password_correct() {
    let scramble = generate_scramble(42);
    let password = "s3cret!";
    let token = compute_auth_token(password, &scramble);
    assert!(verify_mysql_native_password(password, &scramble, &token));
}

#[test]
fn test_verify_mysql_native_password_wrong_password() {
    let scramble = generate_scramble(42);
    let token = compute_auth_token("correct", &scramble);
    assert!(!verify_mysql_native_password("wrong", &scramble, &token));
}

#[test]
fn test_verify_mysql_native_password_empty() {
    let scramble = generate_scramble(42);
    // Empty password + empty auth response should succeed
    assert!(verify_mysql_native_password("", &scramble, &[]));
}

#[test]
fn test_verify_mysql_native_password_bad_length() {
    let scramble = generate_scramble(42);
    // Auth response with wrong length should fail
    assert!(!verify_mysql_native_password("pass", &scramble, &[1, 2, 3]));
}

#[test]
fn test_build_handshake_v10_structure() {
    let scramble = generate_scramble(7);
    let pkt = build_handshake_v10(7, &scramble);

    // Protocol version
    assert_eq!(pkt[0], 10, "protocol version must be 10");

    // Server version (null-terminated)
    let ver_end = pkt[1..].iter().position(|&b| b == 0).unwrap() + 1;
    let version_str = std::str::from_utf8(&pkt[1..ver_end]).unwrap();
    assert_eq!(version_str, SERVER_VERSION);

    // Connection ID
    let cid_start = ver_end + 1; // past null
    let conn_id = u32::from_le_bytes([
        pkt[cid_start],
        pkt[cid_start + 1],
        pkt[cid_start + 2],
        pkt[cid_start + 3],
    ]);
    assert_eq!(conn_id, 7);

    // Extract scramble and verify it matches the input
    let extracted = extract_scramble_from_greeting(&pkt);
    assert_eq!(extracted, scramble);

    // Auth plugin name at end of packet - should end with
    // "mysql_native_password\0"
    let suffix = b"mysql_native_password\x00";
    assert!(
        pkt.ends_with(suffix),
        "handshake must advertise mysql_native_password plugin"
    );
}

#[test]
fn test_build_ok_packet() {
    let pkt = build_ok_packet(5, 10);
    assert_eq!(pkt[0], 0x00, "OK marker");

    // affected_rows = 5 (lenenc int, fits in one byte)
    assert_eq!(pkt[1], 5);

    // last_insert_id = 10
    assert_eq!(pkt[2], 10);

    // status flags = SERVER_STATUS_AUTOCOMMIT (0x0002 LE)
    assert_eq!(pkt[3], 0x02);
    assert_eq!(pkt[4], 0x00);

    // warnings = 0
    assert_eq!(pkt[5], 0x00);
    assert_eq!(pkt[6], 0x00);
}

#[test]
fn test_build_ok_packet_zero() {
    let pkt = build_ok_packet(0, 0);
    assert_eq!(pkt[0], 0x00);
    assert_eq!(pkt[1], 0); // affected_rows
    assert_eq!(pkt[2], 0); // last_insert_id
}

#[test]
fn test_build_err_packet() {
    let pkt = build_err_packet(1064, "42000", "Syntax error");

    assert_eq!(pkt[0], 0xFF, "ERR marker");

    let code = u16::from_le_bytes([pkt[1], pkt[2]]);
    assert_eq!(code, 1064);

    assert_eq!(pkt[3], b'#', "SQL state marker");

    let sql_state = std::str::from_utf8(&pkt[4..9]).unwrap();
    assert_eq!(sql_state, "42000");

    let msg = std::str::from_utf8(&pkt[9..]).unwrap();
    assert_eq!(msg, "Syntax error");
}

#[test]
fn test_build_err_packet_short_state() {
    // State shorter than 5 chars should be zero-padded
    let pkt = build_err_packet(1045, "28", "Access denied");
    let sql_state = &pkt[4..9];
    assert_eq!(sql_state, b"28000");
}

#[test]
fn test_build_eof_packet() {
    let pkt = build_eof_packet();
    assert_eq!(pkt[0], 0xFE, "EOF marker");
    // warnings (2 bytes) + status (2 bytes)
    assert_eq!(pkt.len(), 5);
}

#[test]
fn test_build_column_count() {
    let pkt = build_column_count(3);
    assert_eq!(pkt, encode_lenenc_int(3));
}

#[test]
fn test_build_column_def_structure() {
    let pkt = build_column_def("id", "users", MYSQL_TYPE_LONGLONG, NOT_NULL_FLAG | PRI_KEY_FLAG);

    // Starts with lenenc_str "def" (catalog)
    assert_eq!(pkt[0], 3); // length
    assert_eq!(&pkt[1..4], b"def");

    // Schema = "rustydb"
    assert_eq!(pkt[4], 7);
    assert_eq!(&pkt[5..12], b"rustydb");

    // Virtual table = "users"
    assert_eq!(pkt[12], 5);
    assert_eq!(&pkt[13..18], b"users");

    // Physical table = "users"
    assert_eq!(pkt[18], 5);
    assert_eq!(&pkt[19..24], b"users");

    // Virtual column name = "id"
    assert_eq!(pkt[24], 2);
    assert_eq!(&pkt[25..27], b"id");

    // Physical column name = "id"
    assert_eq!(pkt[27], 2);
    assert_eq!(&pkt[28..30], b"id");
}

#[test]
fn test_build_column_def_type_var_string() {
    let pkt = build_column_def("name", "", MYSQL_TYPE_VAR_STRING, 0);

    // Find the fixed-length section marker (0x0C)
    let marker_pos = pkt.iter().position(|&b| b == 0x0C).unwrap();

    // charset = 45 (utf8mb4 for VAR_STRING)
    let charset = u16::from_le_bytes([pkt[marker_pos + 1], pkt[marker_pos + 2]]);
    assert_eq!(charset, 45);

    // column_length = 65535 for VAR_STRING
    let col_len = u32::from_le_bytes([
        pkt[marker_pos + 3],
        pkt[marker_pos + 4],
        pkt[marker_pos + 5],
        pkt[marker_pos + 6],
    ]);
    assert_eq!(col_len, 65535);

    // column type
    assert_eq!(pkt[marker_pos + 7], MYSQL_TYPE_VAR_STRING);
}

#[test]
fn test_build_text_row_values() {
    let int_val = Value::Integer(42);
    let text_val = Value::Text("hello".to_string());
    let null_val = Value::Null;

    let values: Vec<&Value> = vec![&int_val, &text_val, &null_val];
    let pkt = build_text_row(&values);

    // "42" -> lenenc_str: [2, '4', '2']
    assert_eq!(pkt[0], 2);
    assert_eq!(&pkt[1..3], b"42");

    // "hello" -> lenenc_str: [5, 'h', 'e', 'l', 'l', 'o']
    assert_eq!(pkt[3], 5);
    assert_eq!(&pkt[4..9], b"hello");

    // NULL -> 0xFB
    assert_eq!(pkt[9], 0xFB);
}

#[test]
fn test_build_text_row_boolean() {
    let true_val = Value::Boolean(true);
    let false_val = Value::Boolean(false);
    let values: Vec<&Value> = vec![&true_val, &false_val];
    let pkt = build_text_row(&values);

    // Boolean true -> "1" (MySQL TINY encoding)
    assert_eq!(pkt[0], 1);
    assert_eq!(&pkt[1..2], b"1");

    // Boolean false -> "0"
    assert_eq!(pkt[2], 1);
    assert_eq!(&pkt[3..4], b"0");
}

#[test]
fn test_parse_client_handshake_basic() {
    let response = build_client_handshake_response("testuser", &[0xAA; 20], None);
    let parsed = parse_client_handshake(&response).expect("parse should succeed");

    assert_eq!(parsed.username, "testuser");
    assert_eq!(parsed.auth_response, vec![0xAA; 20]);
    assert!(parsed.database.is_none());
    assert_eq!(
        parsed.auth_plugin.as_deref(),
        Some("mysql_native_password")
    );
}

#[test]
fn test_parse_client_handshake_with_database() {
    let response = build_client_handshake_response("admin", &[0xBB; 20], Some("mydb"));
    let parsed = parse_client_handshake(&response).expect("parse should succeed");

    assert_eq!(parsed.username, "admin");
    assert_eq!(parsed.database.as_deref(), Some("mydb"));
}

#[test]
fn test_parse_client_handshake_empty_auth() {
    let response = build_client_handshake_response("anon", &[], None);
    let parsed = parse_client_handshake(&response).expect("parse should succeed");

    assert_eq!(parsed.username, "anon");
    assert!(parsed.auth_response.is_empty());
}

#[test]
fn test_parse_client_handshake_too_short() {
    let short = vec![0u8; 10];
    let result = parse_client_handshake(&short);
    assert!(result.is_err());
}

#[test]
fn test_parse_client_handshake_capabilities() {
    let response = build_client_handshake_response("u", &[], None);
    let parsed = parse_client_handshake(&response).unwrap();
    // Check that the capabilities we set are present
    assert_ne!(parsed.capabilities & CLIENT_PROTOCOL_41, 0);
    assert_ne!(parsed.capabilities & CLIENT_SECURE_CONNECTION, 0);
    assert_ne!(parsed.capabilities & CLIENT_PLUGIN_AUTH, 0);
}

/// Perform the full MySQL handshake on an already-connected TCP stream.
/// Returns the scramble used by the server.
async fn do_handshake(stream: &mut TcpStream, username: &str, password: &str) -> [u8; 20] {
    // 1. Read the server greeting (seq 0)
    let (seq, greeting) = read_packet(stream).await;
    assert_eq!(seq, 0, "greeting should have sequence id 0");
    assert_eq!(greeting[0], 10, "protocol version must be 10");

    // 2. Extract scramble
    let scramble = extract_scramble_from_greeting(&greeting);

    // 3. Build and send the client handshake response (seq 1)
    let auth_token = compute_auth_token(password, &scramble);
    let response = build_client_handshake_response(username, &auth_token, None);
    write_packet(stream, 1, &response).await;

    // 4. Read OK (seq 2)
    let (seq, ok_pkt) = read_packet(stream).await;
    assert_eq!(seq, 2, "OK should have sequence id 2");
    assert_eq!(ok_pkt[0], 0x00, "expected OK packet after handshake");

    scramble
}

/// Send a COM_QUERY and read the full response (column count, column defs,
/// EOF, rows, EOF).  Returns the row data payloads.
async fn send_query(stream: &mut TcpStream, query: &str) -> Vec<Vec<u8>> {
    // Build COM_QUERY packet: [0x03] + query bytes
    let mut payload = vec![COM_QUERY];
    payload.extend_from_slice(query.as_bytes());
    write_packet(stream, 0, &payload).await;

    // Read first response packet
    let (_seq, first) = read_packet(stream).await;

    // If the first byte is 0x00 it is an OK packet (e.g. DDL / DML)
    if first[0] == 0x00 {
        return Vec::new(); // no rows
    }

    // If the first byte is 0xFF it is an ERR packet
    if first[0] == 0xFF {
        let code = u16::from_le_bytes([first[1], first[2]]);
        let msg = String::from_utf8_lossy(&first[9..]);
        panic!("ERR packet: code={}, msg={}", code, msg);
    }

    // Otherwise it is a column count (lenenc int)
    let col_count = first[0] as usize; // works for counts < 251

    // Read column definitions
    for _ in 0..col_count {
        let (_seq, _col_def) = read_packet(stream).await;
    }

    // Read EOF after column defs
    let (_seq, eof1) = read_packet(stream).await;
    assert_eq!(eof1[0], 0xFE, "expected EOF after column defs");

    // Read rows until EOF
    let mut rows = Vec::new();
    loop {
        let (_seq, row_data) = read_packet(stream).await;
        if row_data[0] == 0xFE && row_data.len() < 9 {
            // EOF packet (marker 0xFE with small length)
            break;
        }
        rows.push(row_data);
    }

    rows
}

/// Send COM_PING and assert OK response.
async fn send_ping(stream: &mut TcpStream) {
    write_packet(stream, 0, &[COM_PING]).await;
    let (_seq, ok) = read_packet(stream).await;
    assert_eq!(ok[0], 0x00, "COM_PING should return OK");
}

/// Send COM_QUIT.
async fn send_quit(stream: &mut TcpStream) {
    write_packet(stream, 0, &[COM_QUIT]).await;
}

/// Helper: decode a lenenc string from a byte slice starting at `pos`.
/// Returns the string and advances `pos` past it.
fn decode_lenenc_str(data: &[u8], pos: &mut usize) -> String {
    let len = data[*pos] as usize;
    *pos += 1;
    let s = String::from_utf8_lossy(&data[*pos..*pos + len]).to_string();
    *pos += len;
    s
}

#[tokio::test]
async fn test_tcp_handshake_select1_ping_quit() {
    let port = random_port().await;
    let state = test_app_state(test_config(port));

    // Spawn the wire server
    let wire_state = Arc::clone(&state);
    tokio::spawn(async move {
        let _ = rustydb::start_wire_server(wire_state, port).await;
    });

    // Give the server a moment to bind
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Connect
    let mut stream = TcpStream::connect(format!("127.0.0.1:{}", port))
        .await
        .expect("connect to wire server");

    // Handshake (no auth)
    do_handshake(&mut stream, "test", "").await;

    // SELECT 1
    let rows = send_query(&mut stream, "SELECT 1").await;
    assert_eq!(rows.len(), 1, "SELECT 1 should return one row");

    // Decode the row value: lenenc_str "1"
    let mut pos = 0;
    let val = decode_lenenc_str(&rows[0], &mut pos);
    assert_eq!(val, "1");

    // PING
    send_ping(&mut stream).await;

    // QUIT
    send_quit(&mut stream).await;
}

#[tokio::test]
async fn test_tcp_auth_success() {
    let port = random_port().await;
    let state = test_app_state(test_config_with_auth(port, "admin", "hunter2"));

    let wire_state = Arc::clone(&state);
    tokio::spawn(async move {
        let _ = rustydb::start_wire_server(wire_state, port).await;
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let mut stream = TcpStream::connect(format!("127.0.0.1:{}", port))
        .await
        .expect("connect");

    // Should succeed with correct credentials
    do_handshake(&mut stream, "admin", "hunter2").await;

    send_ping(&mut stream).await;
    send_quit(&mut stream).await;
}

#[tokio::test]
async fn test_tcp_auth_failure() {
    let port = random_port().await;
    let state = test_app_state(test_config_with_auth(port, "admin", "hunter2"));

    let wire_state = Arc::clone(&state);
    tokio::spawn(async move {
        let _ = rustydb::start_wire_server(wire_state, port).await;
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let mut stream = TcpStream::connect(format!("127.0.0.1:{}", port))
        .await
        .expect("connect");

    // Read greeting
    let (_seq, greeting) = read_packet(&mut stream).await;
    let scramble = extract_scramble_from_greeting(&greeting);

    // Send wrong password
    let bad_token = compute_auth_token("wrongpassword", &scramble);
    let response = build_client_handshake_response("admin", &bad_token, None);
    write_packet(&mut stream, 1, &response).await;

    // Server should respond with ERR packet
    let (_seq, err_pkt) = read_packet(&mut stream).await;
    assert_eq!(err_pkt[0], 0xFF, "expected ERR packet for wrong password");

    let code = u16::from_le_bytes([err_pkt[1], err_pkt[2]]);
    assert_eq!(code, 1045, "error code should be 1045 (access denied)");
}

#[tokio::test]
async fn test_tcp_create_insert_select() {
    let port = random_port().await;
    let state = test_app_state(test_config(port));

    let wire_state = Arc::clone(&state);
    tokio::spawn(async move {
        let _ = rustydb::start_wire_server(wire_state, port).await;
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let mut stream = TcpStream::connect(format!("127.0.0.1:{}", port))
        .await
        .expect("connect");

    do_handshake(&mut stream, "test", "").await;

    // CREATE TABLE
    let rows = send_query(
        &mut stream,
        "CREATE TABLE test_users (id INTEGER PRIMARY KEY, name TEXT, active BOOLEAN)",
    )
    .await;
    assert!(rows.is_empty(), "CREATE TABLE returns OK, no rows");

    // INSERT rows
    let rows = send_query(
        &mut stream,
        "INSERT INTO test_users VALUES (1, 'Alice', true)",
    )
    .await;
    assert!(rows.is_empty(), "INSERT returns OK, no rows");

    let rows = send_query(
        &mut stream,
        "INSERT INTO test_users VALUES (2, 'Bob', false)",
    )
    .await;
    assert!(rows.is_empty());

    let rows = send_query(
        &mut stream,
        "INSERT INTO test_users VALUES (3, 'Carol', true)",
    )
    .await;
    assert!(rows.is_empty());

    // SELECT all rows
    let rows = send_query(&mut stream, "SELECT * FROM test_users").await;
    assert_eq!(rows.len(), 3, "should have 3 rows");

    // Decode first row: id=1, name=Alice, active=true
    let mut pos = 0;
    let id = decode_lenenc_str(&rows[0], &mut pos);
    let name = decode_lenenc_str(&rows[0], &mut pos);
    let active = decode_lenenc_str(&rows[0], &mut pos);
    assert_eq!(id, "1");
    assert_eq!(name, "Alice");
    assert_eq!(active, "1");

    // SELECT with WHERE
    let rows = send_query(
        &mut stream,
        "SELECT name FROM test_users WHERE active = true",
    )
    .await;
    assert_eq!(rows.len(), 2, "two active users");

    send_quit(&mut stream).await;
}

#[tokio::test]
async fn test_tcp_system_queries() {
    let port = random_port().await;
    let state = test_app_state(test_config(port));

    let wire_state = Arc::clone(&state);
    tokio::spawn(async move {
        let _ = rustydb::start_wire_server(wire_state, port).await;
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let mut stream = TcpStream::connect(format!("127.0.0.1:{}", port))
        .await
        .expect("connect");

    do_handshake(&mut stream, "test", "").await;

    // SET command should return OK (no rows)
    let rows = send_query(&mut stream, "SET NAMES utf8mb4").await;
    assert!(rows.is_empty(), "SET should return OK");

    // USE database should return OK
    let rows = send_query(&mut stream, "USE rustydb").await;
    assert!(rows.is_empty(), "USE should return OK");

    // SELECT @@version should return a result set
    let rows = send_query(&mut stream, "SELECT @@version").await;
    assert_eq!(rows.len(), 1, "@@version should return one row");
    let mut pos = 0;
    let version = decode_lenenc_str(&rows[0], &mut pos);
    assert_eq!(version, SERVER_VERSION);

    // SELECT @@version_comment
    let rows = send_query(&mut stream, "SELECT @@version_comment").await;
    assert_eq!(rows.len(), 1);
    let mut pos = 0;
    let comment = decode_lenenc_str(&rows[0], &mut pos);
    assert_eq!(comment, "RustyDB SQL Engine");

    // SHOW DATABASES
    let rows = send_query(&mut stream, "SHOW DATABASES").await;
    assert_eq!(rows.len(), 1);
    let mut pos = 0;
    let db = decode_lenenc_str(&rows[0], &mut pos);
    assert_eq!(db, "rustydb");

    send_quit(&mut stream).await;
}

#[tokio::test]
async fn test_tcp_init_db_and_field_list() {
    let port = random_port().await;
    let state = test_app_state(test_config(port));

    let wire_state = Arc::clone(&state);
    tokio::spawn(async move {
        let _ = rustydb::start_wire_server(wire_state, port).await;
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let mut stream = TcpStream::connect(format!("127.0.0.1:{}", port))
        .await
        .expect("connect");

    do_handshake(&mut stream, "test", "").await;

    // COM_INIT_DB
    let mut payload = vec![COM_INIT_DB];
    payload.extend_from_slice(b"mydb");
    write_packet(&mut stream, 0, &payload).await;
    let (_seq, ok) = read_packet(&mut stream).await;
    assert_eq!(ok[0], 0x00, "COM_INIT_DB should return OK");

    // COM_FIELD_LIST (deprecated, but should return EOF)
    let mut payload = vec![COM_FIELD_LIST];
    payload.extend_from_slice(b"sometable\x00");
    write_packet(&mut stream, 0, &payload).await;
    let (_seq, eof) = read_packet(&mut stream).await;
    assert_eq!(eof[0], 0xFE, "COM_FIELD_LIST should return EOF");

    send_quit(&mut stream).await;
}

#[tokio::test]
async fn test_tcp_unsupported_command() {
    let port = random_port().await;
    let state = test_app_state(test_config(port));

    let wire_state = Arc::clone(&state);
    tokio::spawn(async move {
        let _ = rustydb::start_wire_server(wire_state, port).await;
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let mut stream = TcpStream::connect(format!("127.0.0.1:{}", port))
        .await
        .expect("connect");

    do_handshake(&mut stream, "test", "").await;

    // Send an unsupported command byte (0xFF is not a valid MySQL command)
    write_packet(&mut stream, 0, &[0xFF]).await;
    let (_seq, err) = read_packet(&mut stream).await;
    assert_eq!(err[0], 0xFF, "unsupported command should return ERR");

    let code = u16::from_le_bytes([err[1], err[2]]);
    assert_eq!(code, 1047, "error code 1047 for unsupported command");

    send_quit(&mut stream).await;
}

#[tokio::test]
async fn test_tcp_multiple_queries_same_connection() {
    let port = random_port().await;
    let state = test_app_state(test_config(port));

    let wire_state = Arc::clone(&state);
    tokio::spawn(async move {
        let _ = rustydb::start_wire_server(wire_state, port).await;
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let mut stream = TcpStream::connect(format!("127.0.0.1:{}", port))
        .await
        .expect("connect");

    do_handshake(&mut stream, "test", "").await;

    // Execute many queries in sequence on the same connection
    for i in 1..=10 {
        let rows = send_query(&mut stream, "SELECT 1").await;
        assert_eq!(
            rows.len(),
            1,
            "query {} should return 1 row",
            i
        );
    }

    send_ping(&mut stream).await;
    send_quit(&mut stream).await;
}

#[tokio::test]
async fn test_tcp_transaction_statements() {
    let port = random_port().await;
    let state = test_app_state(test_config(port));

    let wire_state = Arc::clone(&state);
    tokio::spawn(async move {
        let _ = rustydb::start_wire_server(wire_state, port).await;
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let mut stream = TcpStream::connect(format!("127.0.0.1:{}", port))
        .await
        .expect("connect");

    do_handshake(&mut stream, "test", "").await;

    // These are all no-ops but should return OK
    let stmts = ["BEGIN", "START TRANSACTION", "COMMIT", "ROLLBACK"];
    for stmt in &stmts {
        let rows = send_query(&mut stream, stmt).await;
        assert!(rows.is_empty(), "{} should return OK (no rows)", stmt);
    }

    send_quit(&mut stream).await;
}

#[tokio::test]
async fn test_tcp_concurrent_connections() {
    let port = random_port().await;
    let state = test_app_state(test_config(port));

    let wire_state = Arc::clone(&state);
    tokio::spawn(async move {
        let _ = rustydb::start_wire_server(wire_state, port).await;
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Spawn 5 concurrent clients
    let mut handles = Vec::new();
    for i in 0..5 {
        let handle = tokio::spawn(async move {
            let mut stream = TcpStream::connect(format!("127.0.0.1:{}", port))
                .await
                .expect("connect");

            do_handshake(&mut stream, &format!("user{}", i), "").await;

            let rows = send_query(&mut stream, "SELECT 1").await;
            assert_eq!(rows.len(), 1);

            send_ping(&mut stream).await;
            send_quit(&mut stream).await;
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.await.expect("client task should complete");
    }
}

#[tokio::test]
async fn test_tcp_select_database_and_user() {
    let port = random_port().await;
    let state = test_app_state(test_config(port));

    let wire_state = Arc::clone(&state);
    tokio::spawn(async move {
        let _ = rustydb::start_wire_server(wire_state, port).await;
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let mut stream = TcpStream::connect(format!("127.0.0.1:{}", port))
        .await
        .expect("connect");

    do_handshake(&mut stream, "test", "").await;

    // SELECT DATABASE()
    let rows = send_query(&mut stream, "SELECT DATABASE()").await;
    assert_eq!(rows.len(), 1);
    let mut pos = 0;
    let db = decode_lenenc_str(&rows[0], &mut pos);
    assert_eq!(db, "rustydb");

    // SELECT USER()
    let rows = send_query(&mut stream, "SELECT USER()").await;
    assert_eq!(rows.len(), 1);
    let mut pos = 0;
    let user = decode_lenenc_str(&rows[0], &mut pos);
    assert_eq!(user, "rustydb_user@localhost");

    send_quit(&mut stream).await;
}

#[tokio::test]
async fn test_tcp_show_warnings() {
    let port = random_port().await;
    let state = test_app_state(test_config(port));

    let wire_state = Arc::clone(&state);
    tokio::spawn(async move {
        let _ = rustydb::start_wire_server(wire_state, port).await;
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let mut stream = TcpStream::connect(format!("127.0.0.1:{}", port))
        .await
        .expect("connect");

    do_handshake(&mut stream, "test", "").await;

    // SHOW WARNINGS should return an empty result set (3 columns, 0 rows)
    let rows = send_query(&mut stream, "SHOW WARNINGS").await;
    assert_eq!(rows.len(), 0, "SHOW WARNINGS should have no rows");

    send_quit(&mut stream).await;
}
