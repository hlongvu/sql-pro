//! Database drivers for SQL Pro.
//!
//! Mirrors TablePro's driver layer: every engine implements the single
//! [`sqlpro_core::driver::DatabaseDriver`] trait, and a factory resolves a
//! saved connection config to a live session. Engines live behind cargo
//! features so unused engines cost nothing at build time.

pub mod runtime;
pub mod sql_split;

#[cfg(feature = "mysql")]
pub mod mysql;
#[cfg(feature = "postgres")]
pub mod postgres;
#[cfg(feature = "sqlite")]
pub mod sqlite;

use sqlpro_core::connection::ConnectionConfig;
use sqlpro_core::error::{Error, Result};
use sqlpro_core::write::ParameterizedExecutor;

/// Connect to a database described by `config`, returning a live session.
///
/// The returned object is typed as [`ParameterizedExecutor`] — the write
/// path's trait — whose supertrait is [`sqlpro_core::driver::DatabaseDriver`],
/// so reads, schema browsing, and parameterized writes all flow through one
/// handle.
pub fn create_driver(config: &ConnectionConfig) -> Result<Box<dyn ParameterizedExecutor>> {
    sqlpro_core::connection::validate(config)?;
    match config.database_type.driver_id() {
        #[cfg(feature = "sqlite")]
        "sqlite" => Ok(Box::new(sqlite::SqliteDriver::connect(config)?)),
        #[cfg(feature = "postgres")]
        "postgresql" => Ok(Box::new(postgres::PostgresDriver::connect(config)?)),
        #[cfg(feature = "mysql")]
        "mysql" => Ok(Box::new(mysql::MySqlDriver::connect(config)?)),
        other => Err(Error::UnsupportedDatabaseType(other.to_string())),
    }
}
