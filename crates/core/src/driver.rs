use crate::error::Result;
use crate::result::QueryResult;
use crate::schema::{ColumnInfo, TableInfo};

/// One connected database session. Modeled on TablePro's `DatabaseDriver`
/// protocol — the single façade the UI talks to — but async so Rust
/// drivers can be cancelled naturally instead of needing TablePro's
/// abortable-blocking-call machinery for C libraries.
#[async_trait::async_trait]
pub trait DatabaseDriver: Send + Sync {
    /// Liveness check shown as the connection status in the UI.
    async fn ping(&self) -> Result<()>;

    /// Run one statement or a multi-statement script. Row-returning
    /// statements produce `is_query = true` results; multiple row-returning
    /// statements in one script concatenate their rows when schemas match,
    /// otherwise the last statement wins (drivers may differ, documented
    /// per driver).
    async fn execute(&self, sql: &str) -> Result<QueryResult>;

    /// Tables (and views) in the current schema/database, for the sidebar.
    async fn list_tables(&self) -> Result<Vec<TableInfo>>;

    /// Columns of a table, for the schema browser and autocomplete.
    async fn list_columns(&self, table: &str) -> Result<Vec<ColumnInfo>>;

    /// Identifies this connection in logs and the status bar.
    fn label(&self) -> String {
        "database".to_string()
    }
}

/// Registry entry point: builds a driver for a saved connection. The
/// drivers crate provides the real factory; core defines the shape so
/// both UI and drivers depend on core only.
pub trait DriverFactory: Send + Sync {
    /// Which engine this factory builds.
    fn database_type(&self) -> crate::database_type::DatabaseType;
    /// Connect and return a live session.
    fn create(
        &self,
        config: &crate::connection::ConnectionConfig,
    ) -> Result<Box<dyn DatabaseDriver>>;
}
