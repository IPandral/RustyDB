mod kvstore;
mod persistence;
pub mod sql;

#[cfg(feature = "server")]
pub mod server;

#[cfg(feature = "server")]
pub mod wire;

pub use kvstore::KVStore;
pub use persistence::{Operation, PersistenceConfig, PersistenceManager};

pub use sql::{SQLDatabase, SQLConfig, ExecutionResult};
pub use sql::{DataType, Value, ColumnDef, TableSchema, Row, ResultSet};
pub use sql::parse_sql;

#[cfg(feature = "server")]
pub use server::{ServerConfig, start_server, create_router, AppState};

#[cfg(feature = "server")]
pub use wire::start_wire_server;