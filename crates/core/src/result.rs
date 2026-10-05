use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::schema::ColumnInfo;
use crate::value::CellValue;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryResult {
    pub columns: Vec<ColumnInfo>,
    pub rows: Vec<Vec<CellValue>>,
    pub rows_affected: u64,
    pub duration: Duration,
    /// True when the statement produced rows (SELECT-like), false for
    /// write statements (INSERT/UPDATE/...).
    pub is_query: bool,
}

impl QueryResult {
    pub fn empty(duration: Duration) -> Self {
        Self {
            columns: Vec::new(),
            rows: Vec::new(),
            rows_affected: 0,
            duration,
            is_query: false,
        }
    }
}
