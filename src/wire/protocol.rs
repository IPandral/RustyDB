use crate::sql::{DataType, Value};
use sha1::{Digest, Sha1};

// Capability Flags
pub const CLIENT_LONG_PASSWORD: u32 = 0x0000_0001;
pub const CLIENT_FOUND_ROWS: u32 = 0x0000_0002;
pub const CLIENT_LONG_FLAG: u32 = 0x0000_0004;
pub const CLIENT_CONNECT_WITH_DB: u32 = 0x0000_0008;
pub const CLIENT_PROTOCOL_41: u32 = 0x0000_0200;
pub const CLIENT_TRANSACTIONS: u32 = 0x0000_2000;
pub const CLIENT_SECURE_CONNECTION: u32 = 0x0000_8000;
pub const CLIENT_MULTI_RESULTS: u32 = 0x0002_0000;
pub const CLIENT_PLUGIN_AUTH: u32 = 0x0008_0000;
pub const CLIENT_PLUGIN_AUTH_LENENC_CLIENT_DATA: u32 = 0x0020_0000;

// Server Status Flags
pub const SERVER_STATUS_AUTOCOMMIT: u16 = 0x0002;
pub const SERVER_STATUS_IN_TRANS: u16 = 0x0001;

// MySQL Column Types
pub const MYSQL_TYPE_TINY: u8 = 0x01;
pub const MYSQL_TYPE_DOUBLE: u8 = 0x05;
pub const MYSQL_TYPE_LONGLONG: u8 = 0x08;
pub const MYSQL_TYPE_VAR_STRING: u8 = 0xFD;
pub const MYSQL_TYPE_NULL: u8 = 0x06;

// Column Flags
pub const NOT_NULL_FLAG: u16 = 0x0001;
pub const PRI_KEY_FLAG: u16 = 0x0002;
pub const NUM_FLAG: u16 = 0x8000;

// Command Codes
pub const COM_QUIT: u8 = 0x01;
pub const COM_INIT_DB: u8 = 0x02;
pub const COM_QUERY: u8 = 0x03;
pub const COM_FIELD_LIST: u8 = 0x04;
pub const COM_PING: u8 = 0x0E;

// Server version - report as MySQL 5.7 so clients default to mysql_native_password
pub const SERVER_VERSION: &str = "5.7.99-RustyDB-0.3.0-beta";

// Server capabilities to advertise
pub const SERVER_CAPABILITIES: u32 = CLIENT_LONG_PASSWORD
    | CLIENT_FOUND_ROWS
    | CLIENT_LONG_FLAG
    | CLIENT_CONNECT_WITH_DB
    | CLIENT_PROTOCOL_41
    | CLIENT_TRANSACTIONS
    | CLIENT_SECURE_CONNECTION
    | CLIENT_MULTI_RESULTS
    | CLIENT_PLUGIN_AUTH;

// Character set: utf8mb4_general_ci
pub const CHARSET_UTF8MB4: u8 = 45;

// ============================================================================
// Length-Encoded Integer
// ============================================================================

pub fn encode_lenenc_int(val: u64) -> Vec<u8> {
    if val < 251 {
        vec![val as u8]
    } else if val < 65536 {
        let mut buf = vec![0xFC];
        buf.extend_from_slice(&(val as u16).to_le_bytes());
        buf
    } else if val < 16_777_216 {
        let mut buf = vec![0xFD];
        let bytes = (val as u32).to_le_bytes();
        buf.extend_from_slice(&bytes[0..3]);
        buf
    } else {
        let mut buf = vec![0xFE];
        buf.extend_from_slice(&val.to_le_bytes());
        buf
    }
}

// ============================================================================
// Length-Encoded String
// ============================================================================

pub fn encode_lenenc_str(s: &str) -> Vec<u8> {
    let mut buf = encode_lenenc_int(s.len() as u64);
    buf.extend_from_slice(s.as_bytes());
    buf
}

// ============================================================================
// Null-terminated String Helpers
// ============================================================================

pub fn read_null_terminated(data: &[u8], pos: &mut usize) -> String {
    let start = *pos;
    while *pos < data.len() && data[*pos] != 0 {
        *pos += 1;
    }
    let s = String::from_utf8_lossy(&data[start..*pos]).to_string();
    if *pos < data.len() {
        *pos += 1; // skip null terminator
    }
    s
}

// ============================================================================
// Scramble Generation
// ============================================================================

pub fn generate_scramble(conn_id: u32) -> [u8; 20] {
    use std::time::{SystemTime, UNIX_EPOCH};

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();

    let mut data = Vec::with_capacity(32);
    data.extend_from_slice(&conn_id.to_le_bytes());
    data.extend_from_slice(&nanos.to_le_bytes());
    data.extend_from_slice(&std::process::id().to_le_bytes());

    let hash: [u8; 20] = Sha1::digest(&data).into();

    // MySQL scramble must not contain zero bytes
    let mut scramble = hash;
    for b in &mut scramble {
        if *b == 0 {
            *b = 1;
        }
    }
    scramble
}

// ============================================================================
// Authentication
// ============================================================================

/// Verify mysql_native_password authentication.
///
/// The client computes: SHA1(password) XOR SHA1(scramble + SHA1(SHA1(password)))
/// We verify by computing the same from our known plaintext password.
pub fn verify_mysql_native_password(password: &str, scramble: &[u8], client_auth: &[u8]) -> bool {
    if password.is_empty() && client_auth.is_empty() {
        return true;
    }
    if client_auth.len() != 20 {
        return false;
    }

    let stage1: [u8; 20] = Sha1::digest(password.as_bytes()).into();
    let stage2: [u8; 20] = Sha1::digest(stage1).into();

    let mut concat = Vec::with_capacity(scramble.len() + 20);
    concat.extend_from_slice(scramble);
    concat.extend_from_slice(&stage2);
    let scramble_hash: [u8; 20] = Sha1::digest(&concat).into();

    let mut expected = [0u8; 20];
    for i in 0..20 {
        expected[i] = stage1[i] ^ scramble_hash[i];
    }

    expected == client_auth
}

// ============================================================================
// Packet Builders
// ============================================================================

/// Build the server's initial handshake v10 packet payload.
pub fn build_handshake_v10(conn_id: u32, scramble: &[u8; 20]) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(128);

    // Protocol version 10
    pkt.push(10);

    // Server version (null-terminated)
    pkt.extend_from_slice(SERVER_VERSION.as_bytes());
    pkt.push(0);

    // Connection ID (4 bytes LE)
    pkt.extend_from_slice(&conn_id.to_le_bytes());

    // Auth plugin data part 1 (first 8 bytes of scramble)
    pkt.extend_from_slice(&scramble[..8]);

    // Filler
    pkt.push(0);

    // Capability flags (lower 2 bytes)
    pkt.extend_from_slice(&((SERVER_CAPABILITIES & 0xFFFF) as u16).to_le_bytes());

    // Character set
    pkt.push(CHARSET_UTF8MB4);

    // Status flags
    pkt.extend_from_slice(&SERVER_STATUS_AUTOCOMMIT.to_le_bytes());

    // Capability flags (upper 2 bytes)
    pkt.extend_from_slice(&(((SERVER_CAPABILITIES >> 16) & 0xFFFF) as u16).to_le_bytes());

    // Auth plugin data length (21 = 20 bytes scramble + NUL)
    pkt.push(21);

    // Reserved (10 zero bytes)
    pkt.extend_from_slice(&[0u8; 10]);

    // Auth plugin data part 2 (remaining 12 bytes + NUL)
    pkt.extend_from_slice(&scramble[8..20]);
    pkt.push(0);

    // Auth plugin name (null-terminated)
    pkt.extend_from_slice(b"mysql_native_password");
    pkt.push(0);

    pkt
}

/// Build an OK packet payload.
pub fn build_ok_packet(affected_rows: u64, last_insert_id: u64) -> Vec<u8> {
    build_ok_packet_with_status(affected_rows, last_insert_id, SERVER_STATUS_AUTOCOMMIT)
}

pub fn build_ok_packet_with_status(
    affected_rows: u64,
    last_insert_id: u64,
    status: u16,
) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(16);
    pkt.push(0x00); // OK marker
    pkt.extend_from_slice(&encode_lenenc_int(affected_rows));
    pkt.extend_from_slice(&encode_lenenc_int(last_insert_id));
    pkt.extend_from_slice(&status.to_le_bytes()); // status flags
    pkt.extend_from_slice(&0u16.to_le_bytes()); // warnings
    pkt
}

/// Build an ERR packet payload.
pub fn build_err_packet(code: u16, state: &str, msg: &str) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(32 + msg.len());
    pkt.push(0xFF); // ERR marker
    pkt.extend_from_slice(&code.to_le_bytes());
    pkt.push(b'#'); // SQL state marker

    // SQL state (exactly 5 bytes, padded with zeros)
    let state_bytes = state.as_bytes();
    for i in 0..5 {
        pkt.push(if i < state_bytes.len() {
            state_bytes[i]
        } else {
            b'0'
        });
    }

    pkt.extend_from_slice(msg.as_bytes());
    pkt
}

/// Build an EOF packet payload.
pub fn build_eof_packet() -> Vec<u8> {
    build_eof_packet_with_status(SERVER_STATUS_AUTOCOMMIT)
}

pub fn build_eof_packet_with_status(status: u16) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(5);
    pkt.push(0xFE); // EOF marker
    pkt.extend_from_slice(&0u16.to_le_bytes()); // warnings
    pkt.extend_from_slice(&status.to_le_bytes()); // status flags
    pkt
}

/// Build a column count packet payload.
pub fn build_column_count(count: u64) -> Vec<u8> {
    encode_lenenc_int(count)
}

/// Build a column definition packet payload (Protocol::ColumnDefinition41).
pub fn build_column_def(name: &str, table: &str, col_type: u8, col_flags: u16) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(128);

    pkt.extend_from_slice(&encode_lenenc_str("def")); // catalog
    pkt.extend_from_slice(&encode_lenenc_str("rustydb")); // schema
    pkt.extend_from_slice(&encode_lenenc_str(table)); // virtual table
    pkt.extend_from_slice(&encode_lenenc_str(table)); // physical table
    pkt.extend_from_slice(&encode_lenenc_str(name)); // virtual column name
    pkt.extend_from_slice(&encode_lenenc_str(name)); // physical column name

    pkt.push(0x0C); // length of fixed-length fields

    // Character set (2 bytes)
    let charset: u16 = if col_type == MYSQL_TYPE_VAR_STRING {
        45
    } else {
        63
    };
    pkt.extend_from_slice(&charset.to_le_bytes());

    // Column length (4 bytes)
    let col_len: u32 = match col_type {
        MYSQL_TYPE_LONGLONG => 20,
        MYSQL_TYPE_DOUBLE => 22,
        MYSQL_TYPE_TINY => 4,
        MYSQL_TYPE_VAR_STRING => 65535,
        _ => 255,
    };
    pkt.extend_from_slice(&col_len.to_le_bytes());

    pkt.push(col_type); // column type

    pkt.extend_from_slice(&col_flags.to_le_bytes()); // flags

    // Decimals
    pkt.push(if col_type == MYSQL_TYPE_DOUBLE { 31 } else { 0 });

    pkt.extend_from_slice(&[0u8; 2]); // filler

    pkt
}

/// Build a text-protocol row packet payload.
///
/// MySQL's text protocol sends booleans as "1"/"0" (TINY int),
/// not "true"/"false", so we handle that case explicitly.
pub fn build_text_row(values: &[&Value]) -> Vec<u8> {
    let mut pkt = Vec::new();
    for value in values {
        match value {
            Value::Null => pkt.push(0xFB),
            Value::Boolean(b) => {
                let s = if *b { "1" } else { "0" };
                pkt.extend_from_slice(&encode_lenenc_str(s));
            }
            other => {
                let s = other.to_string();
                pkt.extend_from_slice(&encode_lenenc_str(&s));
            }
        }
    }
    pkt
}

/// Map RustyDB DataType to MySQL column type code.
pub fn datatype_to_mysql(dt: &DataType) -> u8 {
    match dt {
        DataType::Integer => MYSQL_TYPE_LONGLONG,
        DataType::Float => MYSQL_TYPE_DOUBLE,
        DataType::Text => MYSQL_TYPE_VAR_STRING,
        DataType::Boolean => MYSQL_TYPE_TINY,
        DataType::Null => MYSQL_TYPE_NULL,
    }
}

/// Compute column flags from a RustyDB column definition.
pub fn column_flags(dt: &DataType, nullable: bool, primary_key: bool) -> u16 {
    let mut flags: u16 = 0;
    if !nullable {
        flags |= NOT_NULL_FLAG;
    }
    if primary_key {
        flags |= PRI_KEY_FLAG | NOT_NULL_FLAG;
    }
    if matches!(dt, DataType::Integer | DataType::Float | DataType::Boolean) {
        flags |= NUM_FLAG;
    }
    flags
}

// ============================================================================
// Client Handshake Parsing
// ============================================================================

/// Parsed client handshake response.
#[allow(dead_code)]
pub struct ClientHandshake {
    pub capabilities: u32,
    pub username: String,
    pub auth_response: Vec<u8>,
    pub database: Option<String>,
    pub auth_plugin: Option<String>,
}

/// Parse a HandshakeResponse41 packet payload.
pub fn parse_client_handshake(data: &[u8]) -> Result<ClientHandshake, String> {
    if data.len() < 32 {
        return Err("Handshake response too short".to_string());
    }

    let mut pos = 0;

    // Capability flags (4 bytes LE)
    let capabilities = u32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]);
    pos += 4;

    // Max packet size (4 bytes) - skip
    pos += 4;

    // Character set (1 byte) - skip
    pos += 1;

    // Reserved (23 zero bytes) - skip
    pos += 23;

    // Username (null-terminated)
    let username = read_null_terminated(data, &mut pos);

    // Auth response
    let auth_response =
        if capabilities & (CLIENT_PLUGIN_AUTH_LENENC_CLIENT_DATA | CLIENT_SECURE_CONNECTION) != 0 {
            if pos >= data.len() {
                Vec::new()
            } else {
                let len = data[pos] as usize;
                pos += 1;
                let end = (pos + len).min(data.len());
                let auth = data[pos..end].to_vec();
                pos = end;
                auth
            }
        } else {
            let start = pos;
            while pos < data.len() && data[pos] != 0 {
                pos += 1;
            }
            let auth = data[start..pos].to_vec();
            if pos < data.len() {
                pos += 1;
            }
            auth
        };

    // Database (if CLIENT_CONNECT_WITH_DB)
    let database = if capabilities & CLIENT_CONNECT_WITH_DB != 0 && pos < data.len() {
        let db = read_null_terminated(data, &mut pos);
        if db.is_empty() { None } else { Some(db) }
    } else {
        None
    };

    // Auth plugin name (if CLIENT_PLUGIN_AUTH)
    let auth_plugin = if capabilities & CLIENT_PLUGIN_AUTH != 0 && pos < data.len() {
        let name = read_null_terminated(data, &mut pos);
        if name.is_empty() { None } else { Some(name) }
    } else {
        None
    };

    Ok(ClientHandshake {
        capabilities,
        username,
        auth_response,
        database,
        auth_plugin,
    })
}
