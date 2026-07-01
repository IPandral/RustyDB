mod kvstore;
mod persistence;
pub mod sql;

#[cfg(feature = "server")]
pub mod server;

#[cfg(feature = "server")]
pub mod wire;

pub use kvstore::KVStore;
pub use persistence::{Operation, PersistenceConfig, PersistenceManager};

pub use sql::parse_sql;
pub use sql::{ColumnDef, DataType, ResultSet, Row, TableSchema, Value};
pub use sql::{
    ExecutionResult, SQLConfig, SQLDatabase, SQLDatabaseOptions, SQLSession,
    SessionTransactionState,
};
pub use sql::{LogicalPlan, Optimizer, Planner};

#[cfg(feature = "server")]
pub use server::{AppState, ServerConfig, create_router, start_server};

#[cfg(feature = "server")]
pub use wire::start_wire_server;
