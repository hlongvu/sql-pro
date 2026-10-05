use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableInfo {
    pub name: String,
    pub is_view: bool,
    /// Approximate row count when the engine reports it cheaply.
    pub row_estimate: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColumnInfo {
    pub name: String,
    /// Engine-reported type, e.g. "int4" (Postgres) or "INTEGER" (SQLite).
    pub data_type: String,
    pub nullable: bool,
    pub is_primary_key: bool,
}
