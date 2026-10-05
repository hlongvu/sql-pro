//! SQL Pro — shared domain models and the database driver abstraction.
//!
//! Ported from TablePro's architecture: a string-backed, open [`DatabaseType`],
//! a single [`DatabaseDriver`] trait every engine implements, and value
//! models shared by drivers and UI.

pub mod change;
pub mod connection;
pub mod csv;
pub mod database_type;
pub mod driver;
pub mod error;
pub mod fingerprint;
pub mod history;
pub mod result;
pub mod schema;
pub mod sqlgen;
pub mod value;
pub mod write;

pub use change::{CellChange, ChangeType, FieldAbsence, PendingChanges, RowChange, RowID};
pub use connection::{validate, ConnectionConfig, SshAuth, SshTunnelConfig};
pub use database_type::DatabaseType;
pub use driver::{DatabaseDriver, DriverFactory};
pub use error::{Error, Result};
pub use result::QueryResult;
pub use schema::{ColumnInfo, TableInfo};
pub use sqlgen::{ParameterStyle, ParameterizedStatement, SqlStatementGenerator};
pub use value::{BindValue, CellValue};
pub use write::{DataWriteExecutor, DataWritePlan, ParameterizedExecutor, TransactionOwner};
