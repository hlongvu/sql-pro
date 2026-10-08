//! A database engine type.
//!
//! Modeled on TablePro's `DatabaseType`: an open, string-backed type so
//! plugin drivers can register engines this binary has never heard of and
//! connection configs still round-trip.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DatabaseType(String);

impl DatabaseType {
    pub const SQLITE: &str = "sqlite";
    pub const POSTGRESQL: &str = "postgresql";
    pub const MYSQL: &str = "mysql";

    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn id(&self) -> &str {
        &self.0
    }

    /// Whether this binary ships a driver for this engine.
    pub fn is_builtin(&self) -> bool {
        matches!(
            self.id(),
            "sqlite" | "postgresql" | "mysql" | "mariadb" | "redshift" | "cockroachdb"
        )
    }

    /// Protocol-compatible aliases of a known engine, like TablePro treats
    /// MariaDB vs MySQL or Redshift vs PostgreSQL.
    pub fn driver_id(&self) -> &str {
        match self.id() {
            "mariadb" => "mysql",
            "redshift" | "cockroachdb" => "postgresql",
            other => other,
        }
    }

    /// Default TCP port for the engine, if it has one.
    pub fn default_port(&self) -> Option<u16> {
        // Protocol-compatible aliases first: cockroach/redshift are PG
        // wire-compatible but listen on their own ports.
        match self.id() {
            "cockroachdb" => return Some(26257),
            "redshift" => return Some(5439),
            "mssql" => return Some(1433),
            "cassandra" => return Some(9042),
            "mongodb" => return Some(27017),
            "snowflake" => return Some(443),
            "redis" => return Some(6379),
            "oracle" => return Some(1521),
            "vertica" => return Some(5433),
            _ => {}
        }
        match self.driver_id() {
            "postgresql" => Some(5432),
            "mysql" => Some(3306),
            _ => None,
        }
    }
}

impl std::fmt::Display for DatabaseType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for DatabaseType {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl From<&str> for DatabaseType {
    fn from(s: &str) -> Self {
        Self(s.into())
    }
}
