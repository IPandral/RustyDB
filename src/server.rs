use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, post, put},
};
use base64::Engine;
use hyper::server::conn::http1;
use hyper_util::{rt::TokioIo, service::TowerToHyperService};
use serde::{Deserialize, Serialize};
use std::env;
use std::io;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use subtle::ConstantTimeEq;
use tokio::net::TcpListener;
use tokio_rustls::{
    TlsAcceptor, rustls,
    rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject},
};
use tower_http::cors::{Any, CorsLayer};
use zeroize::Zeroizing;

use crate::kvstore::KVStore;
use crate::sql::{ExecutionResult, SQLDatabase, SQLDatabaseOptions};
use crate::{RUSTYDB_VERSION, print_outdated_version_warning};

fn parse_bloom_false_positive_rate(value: Option<String>) -> f64 {
    value
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value > 0.0 && *value < 1.0)
        .unwrap_or(0.01)
}

/// Server configuration from environment variables
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
    pub tls_cert: Option<PathBuf>,
    pub tls_key: Option<PathBuf>,
    pub allow_unauthenticated_local: bool,
    pub data_dir: String,
    pub memory_only: bool,
    pub wire_port: u16,
    pub plan_cache_capacity: usize,
    pub memory_pool_capacity: usize,
    pub bloom_false_positive_rate: f64,
}

impl ServerConfig {
    /// Load configuration from environment variables
    pub fn from_env() -> Self {
        ServerConfig {
            host: env::var("RUSTYDB_HOST").unwrap_or_else(|_| "0.0.0.0".to_string()),
            port: env::var("RUSTYDB_PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or(8080),
            username: env::var("RUSTYDB_USERNAME").ok(),
            password: env::var("RUSTYDB_PASSWORD").ok(),
            tls_cert: env::var_os("RUSTYDB_TLS_CERT").map(PathBuf::from),
            tls_key: env::var_os("RUSTYDB_TLS_KEY").map(PathBuf::from),
            allow_unauthenticated_local: matches!(
                env::var("RUSTYDB_ALLOW_UNAUTHENTICATED_LOCAL").as_deref(),
                Ok("true" | "1")
            ),
            data_dir: env::var("RUSTYDB_DATA_DIR").unwrap_or_else(|_| "./rustydb_data".to_string()),
            memory_only: env::var("RUSTYDB_MEMORY_ONLY")
                .map(|v| v == "true" || v == "1")
                .unwrap_or(false),
            wire_port: env::var("RUSTYDB_WIRE_PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or(3307),
            plan_cache_capacity: env::var("RUSTYDB_PLAN_CACHE_CAPACITY")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(256),
            memory_pool_capacity: env::var("RUSTYDB_MEMORY_POOL_CAPACITY")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(32),
            bloom_false_positive_rate: parse_bloom_false_positive_rate(
                env::var("RUSTYDB_BLOOM_FALSE_POSITIVE_RATE").ok(),
            ),
        }
    }

    /// Check if authentication is required
    pub fn auth_required(&self) -> bool {
        !self.local_access_allowed()
    }

    fn local_access_allowed(&self) -> bool {
        self.allow_unauthenticated_local
            && self.host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
            && self.username.is_none()
            && self.password.is_none()
            && self.tls_cert.is_none()
            && self.tls_key.is_none()
    }

    pub(crate) fn credentials_match(&self, username: &[u8], password: &[u8]) -> bool {
        let Some((expected_user, expected_password)) =
            self.username.as_ref().zip(self.password.as_ref())
        else {
            return false;
        };
        bool::from(
            expected_user.as_bytes().ct_eq(username) & expected_password.as_bytes().ct_eq(password),
        )
    }

    pub(crate) fn tls_acceptor(&self) -> Result<Option<TlsAcceptor>, Box<dyn std::error::Error>> {
        if self.local_access_allowed() {
            return Ok(None);
        }
        if self.allow_unauthenticated_local {
            return Err("Unauthenticated mode requires a numeric loopback bind address and no credentials or TLS configuration".into());
        }
        if !self
            .username
            .as_deref()
            .is_some_and(|name| !name.trim().is_empty() && !name.contains([':', '\0']))
            || !self.password.as_deref().is_some_and(|password| {
                !password.trim().is_empty()
                    && !password.contains('\0')
                    && password.len() <= crate::wire::MAX_PASSWORD_BYTES
            })
        {
            return Err("Set a nonempty RUSTYDB_USERNAME and RUSTYDB_PASSWORD with valid MySQL and HTTP Basic credentials".into());
        }
        let Some((cert, key)) = self.tls_cert.as_ref().zip(self.tls_key.as_ref()) else {
            return Err(
                "Authenticated listeners require RUSTYDB_TLS_CERT and RUSTYDB_TLS_KEY".into(),
            );
        };
        let certificates = CertificateDer::pem_file_iter(cert)?.collect::<Result<Vec<_>, _>>()?;
        let key = PrivateKeyDer::from_pem_file(key)?;
        let tls = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(certificates, key)?;
        Ok(Some(TlsAcceptor::from(Arc::new(tls))))
    }
}

/// Shared state for the server
pub struct AppState {
    pub kv_store: KVStore,
    pub sql_db: SQLDatabase,
    pub config: ServerConfig,
}

#[derive(Debug, Deserialize)]
pub struct SetRequest {
    pub value: String,
}

#[derive(Debug, Deserialize)]
pub struct SetManyRequest {
    pub pairs: Vec<KeyValuePair>,
}

#[derive(Debug, Deserialize)]
pub struct KeyValuePair {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Deserialize)]
pub struct GetManyRequest {
    pub keys: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct SqlRequest {
    pub query: String,
}

#[derive(Debug, Serialize)]
pub struct ApiResponse<T: Serialize> {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl<T: Serialize> ApiResponse<T> {
    pub fn success(data: T) -> Self {
        ApiResponse {
            success: true,
            data: Some(data),
            error: None,
        }
    }

    pub fn error(msg: &str) -> ApiResponse<()> {
        ApiResponse {
            success: false,
            data: None,
            error: Some(msg.to_string()),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ValueResponse {
    pub key: String,
    pub value: Option<String>,
    pub exists: bool,
}

#[derive(Debug, Serialize)]
pub struct StoreInfoResponse {
    pub keys_count: usize,
    pub is_empty: bool,
    pub wal_size_bytes: u64,
}

#[derive(Debug, Serialize)]
pub struct SqlResultResponse {
    pub query: String,
    pub result_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rows_affected: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tables: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub columns: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rows: Option<Vec<Vec<serde_json::Value>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema: Option<Vec<ColumnInfo>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ColumnInfo {
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    pub primary_key: bool,
}

async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    if !state.config.auth_required() {
        return next.run(request).await;
    }

    let auth_header = match headers.get(header::AUTHORIZATION) {
        Some(h) => h,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                [(header::WWW_AUTHENTICATE, "Basic realm=\"RustyDB\"")],
                Json(ApiResponse::<()>::error("Authentication required")),
            )
                .into_response();
        }
    };

    let auth_str = match auth_header.to_str() {
        Ok(s) => s,
        Err(_) => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(ApiResponse::<()>::error("Invalid authorization header")),
            )
                .into_response();
        }
    };

    if !auth_str.starts_with("Basic ") {
        return (
            StatusCode::UNAUTHORIZED,
            Json(ApiResponse::<()>::error("Basic authentication required")),
        )
            .into_response();
    }

    let encoded = &auth_str[6..];
    let decoded = match base64::engine::general_purpose::STANDARD.decode(encoded) {
        Ok(d) => Zeroizing::new(d),
        Err(_) => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(ApiResponse::<()>::error("Invalid base64 encoding")),
            )
                .into_response();
        }
    };

    let credentials = match std::str::from_utf8(&decoded) {
        Ok(s) => s,
        Err(_) => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(ApiResponse::<()>::error("Invalid UTF-8 in credentials")),
            )
                .into_response();
        }
    };

    let Some((user, pass)) = credentials.split_once(':') else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(ApiResponse::<()>::error("Invalid credentials format")),
        )
            .into_response();
    };

    if !state
        .config
        .credentials_match(user.as_bytes(), pass.as_bytes())
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(ApiResponse::<()>::error("Invalid username or password")),
        )
            .into_response();
    }

    next.run(request).await
}

/// GET /kv/:key - Get a value by key
async fn kv_get(State(state): State<Arc<AppState>>, Path(key): Path<String>) -> impl IntoResponse {
    match state.kv_store.get(&key) {
        Ok(Some(value)) => (
            StatusCode::OK,
            Json(ApiResponse::success(ValueResponse {
                key,
                value: Some(value.as_ref().clone()),
                exists: true,
            })),
        ),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(ApiResponse::success(ValueResponse {
                key,
                value: None,
                exists: false,
            })),
        ),
        Err(_e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::success(ValueResponse {
                key,
                value: None,
                exists: false,
            })),
        ),
    }
}

/// PUT /kv/:key - Set a value (sync)
async fn kv_set(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
    Json(body): Json<SetRequest>,
) -> impl IntoResponse {
    match state.kv_store.set(key.clone(), body.value.clone()) {
        Ok(_) => (
            StatusCode::OK,
            Json(ApiResponse::success(ValueResponse {
                key,
                value: Some(body.value),
                exists: true,
            })),
        ),
        Err(_e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::success(ValueResponse {
                key,
                value: None,
                exists: false,
            })),
        ),
    }
}

/// POST /kv/:key - Set a value (async)
async fn kv_set_async(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
    Json(body): Json<SetRequest>,
) -> impl IntoResponse {
    match state.kv_store.set_async(key.clone(), body.value.clone()) {
        Ok(_) => (
            StatusCode::OK,
            Json(ApiResponse::success(ValueResponse {
                key,
                value: Some(body.value),
                exists: true,
            })),
        ),
        Err(_e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::success(ValueResponse {
                key,
                value: None,
                exists: false,
            })),
        ),
    }
}

/// DELETE /kv/:key - Delete a key
async fn kv_delete(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
) -> impl IntoResponse {
    match state.kv_store.delete(&key) {
        Ok(deleted) => {
            let status = if deleted {
                StatusCode::OK
            } else {
                StatusCode::NOT_FOUND
            };
            (
                status,
                Json(ApiResponse::success(serde_json::json!({
                    "key": key,
                    "deleted": deleted
                }))),
            )
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::success(serde_json::json!({
                "key": key,
                "deleted": false,
                "error": e
            }))),
        ),
    }
}

/// POST /kv/mget - Get multiple keys
async fn kv_get_many(
    State(state): State<Arc<AppState>>,
    Json(body): Json<GetManyRequest>,
) -> impl IntoResponse {
    let keys: Vec<&str> = body.keys.iter().map(|s| s.as_str()).collect();
    match state.kv_store.get_many(&keys) {
        Ok(results) => {
            let data: Vec<ValueResponse> = body
                .keys
                .iter()
                .map(|key| {
                    let value = results
                        .iter()
                        .find(|(k, _)| k == key)
                        .map(|(_, v)| v.as_ref().clone());
                    ValueResponse {
                        key: key.clone(),
                        value: value.clone(),
                        exists: value.is_some(),
                    }
                })
                .collect();
            (StatusCode::OK, Json(ApiResponse::success(data)))
        }
        Err(_e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::success(vec![] as Vec<ValueResponse>)),
        ),
    }
}

/// POST /kv/mset - Set multiple key-value pairs
async fn kv_set_many(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SetManyRequest>,
) -> impl IntoResponse {
    let pairs: Vec<(String, String)> = body.pairs.into_iter().map(|p| (p.key, p.value)).collect();
    let count = pairs.len();

    match state.kv_store.set_many(pairs) {
        Ok(set_count) => (
            StatusCode::OK,
            Json(ApiResponse::success(serde_json::json!({
                "requested": count,
                "set": set_count
            }))),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::success(serde_json::json!({
                "error": e
            }))),
        ),
    }
}

/// GET /kv/info - Get store information
async fn kv_info(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let keys_count = state.kv_store.len().unwrap_or(0);
    let is_empty = state.kv_store.is_empty().unwrap_or(true);
    let wal_size_bytes = state.kv_store.wal_size().unwrap_or(0);

    (
        StatusCode::OK,
        Json(ApiResponse::success(StoreInfoResponse {
            keys_count,
            is_empty,
            wal_size_bytes,
        })),
    )
}

/// POST /kv/clear - Clear all keys
async fn kv_clear(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    match state.kv_store.clear() {
        Ok(_) => (
            StatusCode::OK,
            Json(ApiResponse::success(serde_json::json!({
                "cleared": true
            }))),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::success(serde_json::json!({
                "cleared": false,
                "error": e
            }))),
        ),
    }
}

/// POST /kv/snapshot - Create a snapshot
async fn kv_snapshot(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    match state.kv_store.snapshot() {
        Ok(_) => (
            StatusCode::OK,
            Json(ApiResponse::success(serde_json::json!({
                "snapshot": true
            }))),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::success(serde_json::json!({
                "snapshot": false,
                "error": e
            }))),
        ),
    }
}

/// POST /kv/flush - Flush pending writes
async fn kv_flush(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    match state.kv_store.flush() {
        Ok(_) => (
            StatusCode::OK,
            Json(ApiResponse::success(serde_json::json!({
                "flushed": true
            }))),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::success(serde_json::json!({
                "flushed": false,
                "error": e
            }))),
        ),
    }
}

fn value_to_json(value: &crate::sql::Value) -> serde_json::Value {
    match value {
        crate::sql::Value::Null => serde_json::Value::Null,
        crate::sql::Value::Integer(i) => serde_json::Value::Number((*i).into()),
        crate::sql::Value::Float(f) => serde_json::Number::from_f64(*f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        crate::sql::Value::Text(s) => serde_json::Value::String(s.clone()),
        crate::sql::Value::Boolean(b) => serde_json::Value::Bool(*b),
    }
}

/// POST /sql/execute - Execute a SQL query
async fn sql_execute(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SqlRequest>,
) -> impl IntoResponse {
    let result = state.sql_db.execute(&body.query);

    let response = match result {
        ExecutionResult::Select(rs) => SqlResultResponse {
            query: body.query,
            result_type: "select".to_string(),
            rows_affected: None,
            table_name: None,
            tables: None,
            columns: Some(rs.columns.clone()),
            rows: Some(
                rs.rows
                    .iter()
                    .map(|row| row.values.iter().map(value_to_json).collect())
                    .collect(),
            ),
            schema: None,
            message: Some(format!("{} row(s) returned", rs.rows.len())),
        },
        ExecutionResult::RowsAffected(n) => SqlResultResponse {
            query: body.query,
            result_type: "rows_affected".to_string(),
            rows_affected: Some(n),
            table_name: None,
            tables: None,
            columns: None,
            rows: None,
            schema: None,
            message: Some(format!("{} row(s) affected", n)),
        },
        ExecutionResult::TableCreated(name) => SqlResultResponse {
            query: body.query,
            result_type: "table_created".to_string(),
            rows_affected: None,
            table_name: Some(name.clone()),
            tables: None,
            columns: None,
            rows: None,
            schema: None,
            message: Some(format!("Table '{}' created", name)),
        },
        ExecutionResult::TableDropped(name) => SqlResultResponse {
            query: body.query,
            result_type: "table_dropped".to_string(),
            rows_affected: None,
            table_name: Some(name.clone()),
            tables: None,
            columns: None,
            rows: None,
            schema: None,
            message: Some(format!("Table '{}' dropped", name)),
        },
        ExecutionResult::IndexCreated(name) => SqlResultResponse {
            query: body.query,
            result_type: "index_created".to_string(),
            rows_affected: None,
            table_name: Some(name.clone()),
            tables: None,
            columns: None,
            rows: None,
            schema: None,
            message: Some(format!("Index '{}' created", name)),
        },
        ExecutionResult::IndexDropped(name) => SqlResultResponse {
            query: body.query,
            result_type: "index_dropped".to_string(),
            rows_affected: None,
            table_name: Some(name.clone()),
            tables: None,
            columns: None,
            rows: None,
            schema: None,
            message: Some(format!("Index '{}' dropped", name)),
        },
        ExecutionResult::Tables(tables) => SqlResultResponse {
            query: body.query,
            result_type: "tables".to_string(),
            rows_affected: None,
            table_name: None,
            tables: Some(tables),
            columns: None,
            rows: None,
            schema: None,
            message: None,
        },
        ExecutionResult::TableDescription {
            table_name,
            columns,
        } => SqlResultResponse {
            query: body.query,
            result_type: "describe".to_string(),
            rows_affected: None,
            table_name: Some(table_name),
            tables: None,
            columns: None,
            rows: None,
            schema: Some(
                columns
                    .iter()
                    .map(|c| ColumnInfo {
                        name: c.name.clone(),
                        data_type: c.data_type.to_string(),
                        nullable: c.nullable,
                        primary_key: c.primary_key,
                    })
                    .collect(),
            ),
            message: None,
        },
        ExecutionResult::Indexes(indexes) => SqlResultResponse {
            query: body.query,
            result_type: "indexes".to_string(),
            rows_affected: None,
            table_name: None,
            tables: None,
            columns: Some(vec![
                "name".to_string(),
                "columns".to_string(),
                "unique".to_string(),
            ]),
            rows: Some(
                indexes
                    .into_iter()
                    .map(|index| {
                        vec![
                            serde_json::Value::String(index.name),
                            serde_json::Value::String(index.columns.join(", ")),
                            serde_json::Value::Bool(index.unique),
                        ]
                    })
                    .collect(),
            ),
            schema: None,
            message: None,
        },
        ExecutionResult::Explain(lines) => SqlResultResponse {
            query: body.query,
            result_type: "explain".to_string(),
            rows_affected: None,
            table_name: None,
            tables: None,
            columns: Some(vec!["plan".to_string()]),
            rows: Some(
                lines
                    .into_iter()
                    .map(|line| vec![serde_json::Value::String(line)])
                    .collect(),
            ),
            schema: None,
            message: None,
        },
        transaction_result @ (ExecutionResult::TransactionStarted
        | ExecutionResult::TransactionCommitted
        | ExecutionResult::TransactionRolledBack) => SqlResultResponse {
            query: body.query,
            result_type: "transaction".to_string(),
            rows_affected: None,
            table_name: None,
            tables: None,
            columns: None,
            rows: None,
            schema: None,
            message: Some(transaction_result.to_string()),
        },
        ExecutionResult::Error(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<SqlResultResponse>::error(&e)),
            )
                .into_response();
        }
    };

    (StatusCode::OK, Json(ApiResponse::success(response))).into_response()
}

/// GET /sql/tables - List all tables
async fn sql_tables(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let tables = state.sql_db.list_tables();
    (
        StatusCode::OK,
        Json(ApiResponse::success(serde_json::json!({
            "tables": tables,
            "count": tables.len()
        }))),
    )
}

/// POST /sql/save - Save database to disk
async fn sql_save(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    match state.sql_db.save() {
        Ok(_) => (
            StatusCode::OK,
            Json(ApiResponse::success(serde_json::json!({
                "saved": true
            }))),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::success(serde_json::json!({
                "saved": false,
                "error": e
            }))),
        ),
    }
}

async fn health_check() -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(ApiResponse::success(serde_json::json!({
            "status": "healthy",
            "version": RUSTYDB_VERSION
        }))),
    )
}

pub fn create_router(state: Arc<AppState>) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let kv_routes = Router::new()
        .route("/{key}", get(kv_get))
        .route("/{key}", put(kv_set))
        .route("/{key}", post(kv_set_async))
        .route("/{key}", delete(kv_delete))
        .route("/", post(kv_set_many))
        .route("/mget", post(kv_get_many))
        .route("/mset", post(kv_set_many))
        .route("/info", get(kv_info))
        .route("/clear", post(kv_clear))
        .route("/snapshot", post(kv_snapshot))
        .route("/flush", post(kv_flush));

    let sql_routes = Router::new()
        .route("/execute", post(sql_execute))
        .route("/tables", get(sql_tables))
        .route("/save", post(sql_save));

    let protected_routes = Router::new()
        .nest("/kv", kv_routes)
        .nest("/sql", sql_routes)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ));

    Router::new()
        .route("/health", get(health_check))
        .merge(protected_routes)
        .layer(cors)
        .with_state(state)
}

async fn serve_https(listener: TcpListener, app: Router, tls: TlsAcceptor) -> io::Result<()> {
    let handshake_slots = Arc::new(tokio::sync::Semaphore::new(128));
    loop {
        let (stream, _) = listener.accept().await?;
        let Ok(slot) = Arc::clone(&handshake_slots).try_acquire_owned() else {
            continue;
        };
        let tls = tls.clone();
        let app = app.clone();
        tokio::spawn(async move {
            let handshake = tokio::time::timeout(Duration::from_secs(10), tls.accept(stream)).await;
            drop(slot);
            if let Ok(Ok(stream)) = handshake {
                let _ = http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), TowerToHyperService::new(app))
                    .await;
            }
        });
    }
}

/// Start the HTTP and MySQL listeners after validating transport security.
pub async fn start_server(config: ServerConfig) -> Result<(), Box<dyn std::error::Error>> {
    let tls = config.tls_acceptor()?;
    let http_listener = TcpListener::bind((config.host.as_str(), config.port)).await?;
    let wire_listener = TcpListener::bind((config.host.as_str(), config.wire_port)).await?;
    let kv_store = if config.memory_only {
        KVStore::new()
    } else {
        KVStore::open(&config.data_dir).unwrap_or_else(|_| KVStore::new())
    };

    let sql_options = SQLDatabaseOptions {
        plan_cache_capacity: config.plan_cache_capacity,
        memory_pool_capacity: config.memory_pool_capacity,
        bloom_false_positive_rate: config.bloom_false_positive_rate,
    };
    let sql_db = if config.memory_only {
        SQLDatabase::with_options(sql_options)
    } else {
        SQLDatabase::open_with_options(&config.data_dir, sql_options.clone())
            .unwrap_or_else(|_| SQLDatabase::with_options(sql_options))
    };

    let (scheme, auth_msg) = if tls.is_some() {
        ("https", "TLS with authentication required")
    } else {
        ("http", "Unauthenticated local development mode")
    };

    let wire_addr = wire_listener.local_addr()?;
    let addr = http_listener.local_addr()?;
    println!("RustyDB REST API Server v{}", RUSTYDB_VERSION);
    println!("=================================");
    print_outdated_version_warning();
    println!("  HTTP API:   {scheme}://{addr}");
    println!("  MySQL Wire: {wire_addr}");
    println!("  Auth:      {}", auth_msg);
    println!("  Data dir:  {}", config.data_dir);
    println!(
        "  Memory:    {}",
        if config.memory_only { "YES" } else { "NO" }
    );
    println!();
    println!("HTTP Endpoints:");
    println!("  GET    /health           - Health check");
    println!("  GET    /kv/:key          - Get value");
    println!("  PUT    /kv/:key          - Set value (sync)");
    println!("  POST   /kv/:key          - Set value (async)");
    println!("  DELETE /kv/:key          - Delete key");
    println!("  POST   /kv/mget          - Get multiple keys");
    println!("  POST   /kv/mset          - Set multiple keys");
    println!("  GET    /kv/info          - Store info");
    println!("  POST   /kv/clear         - Clear all keys");
    println!("  POST   /kv/snapshot      - Create snapshot");
    println!("  POST   /kv/flush         - Flush pending writes");
    println!("  POST   /sql/execute      - Execute SQL query");
    println!("  GET    /sql/tables       - List tables");
    println!("  POST   /sql/save         - Save database");
    println!();
    let state = Arc::new(AppState {
        kv_store,
        sql_db,
        config,
    });

    let app = create_router(state.clone());

    println!("Server started! Press Ctrl+C to stop.\n");
    let wire = crate::wire::serve_wire(wire_listener, state, tls.clone());
    let http = async {
        match tls {
            Some(tls) => serve_https(http_listener, app, tls).await,
            None => axum::serve(http_listener, app).await,
        }
    };
    tokio::try_join!(wire, http)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::parse_bloom_false_positive_rate;

    #[test]
    fn rejects_invalid_bloom_false_positive_rates() {
        for value in ["-1", "0", "1", "2", "NaN", "inf"] {
            assert_eq!(
                parse_bloom_false_positive_rate(Some(value.to_string())),
                0.01
            );
        }
    }

    #[test]
    fn accepts_valid_bloom_false_positive_rate() {
        assert_eq!(
            parse_bloom_false_positive_rate(Some("0.05".to_string())),
            0.05
        );
    }
}
