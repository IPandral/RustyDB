mod kvstore;
mod lock;
mod persistence;
pub mod recovery;
pub mod sql;
mod version_check;

#[cfg(feature = "server")]
pub mod server;

#[cfg(feature = "server")]
pub mod wire;

pub use kvstore::KVStore;
pub use persistence::{Operation, PersistenceConfig, PersistenceManager};
pub use recovery::{
    BackupFile, BackupManager, BackupManifest, PruneReport, RecoveryPoint, RecoveryTarget,
    parse_rfc3339,
};

pub use sql::parse_sql;
pub use sql::{AlterOperation, InsertSource, Statement};
pub use sql::{ColumnDef, DataType, ResultSet, Row, TableSchema, Value};
pub use sql::{
    ExecutionResult, PreparedStatement, SQLConfig, SQLDatabase, SQLDatabaseOptions, SQLSession,
    SessionTransactionState,
};
pub use sql::{LogicalPlan, Optimizer, Planner};
pub use version_check::print_outdated_version_warning;

pub const RUSTYDB_VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(feature = "server")]
pub use server::{AppState, ServerConfig, create_router, start_server};

#[cfg(feature = "server")]
pub use wire::start_wire_server;
