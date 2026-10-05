use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("connection failed: {0}")]
    Connection(String),

    #[error("query failed: {0}")]
    Query(String),

    #[error("unsupported database type: {0}")]
    UnsupportedDatabaseType(String),

    #[error("driver error: {0}")]
    Driver(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    Other(String),
}
