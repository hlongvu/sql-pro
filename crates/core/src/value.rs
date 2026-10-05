//! A single value in a result grid or a pending edit.
//!
//! Ported from TablePro's `PluginCellValue`: the ONLY three states anything
//! cell-shaped can be. There is no int/float/date variant — all non-binary
//! values are text; typing/formatting happens above this layer, and SQL
//! binding sends the raw string so the server coerces it.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "lowercase")]
pub enum CellValue {
    Null,
    Text(String),
    Bytes(Vec<u8>),
}

impl CellValue {
    /// Convenience constructor matching Swift's string-literal support.
    pub fn text(s: impl Into<String>) -> Self {
        CellValue::Text(s.into())
    }

    pub fn bytes(b: impl Into<Vec<u8>>) -> Self {
        CellValue::Bytes(b.into())
    }

    pub fn from_optional(s: Option<&str>) -> Self {
        s.map(|v| CellValue::Text(v.to_string())).unwrap_or(CellValue::Null)
    }

    pub fn is_null(&self) -> bool {
        matches!(self, CellValue::Null)
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            CellValue::Text(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            CellValue::Bytes(b) => Some(b),
            _ => None,
        }
    }

    /// String representation suitable for sorting and equality comparison.
    /// Binary cells are rendered as uppercase hex without prefix so
    /// byte-wise lexicographic order matches a stable sort across runs.
    pub fn sort_key(&self) -> String {
        match self {
            CellValue::Null => String::new(),
            CellValue::Text(s) => s.clone(),
            CellValue::Bytes(b) => b.iter().map(|byte| format!("{byte:02X}")).collect(),
        }
    }

    /// Text shown in the grid.
    pub fn display(&self) -> String {
        match self {
            CellValue::Null => String::new(),
            CellValue::Text(s) => s.clone(),
            CellValue::Bytes(_) => "<binary>".to_string(),
        }
    }
}

/// A value bound into a parameterized statement: the `Any?` of TablePro's
/// `ParameterizedStatement.parameters`.
#[derive(Debug, Clone, PartialEq)]
pub enum BindValue {
    Text(String),
    Bytes(Vec<u8>),
}

impl From<&CellValue> for Option<BindValue> {
    fn from(value: &CellValue) -> Self {
        match value {
            CellValue::Null => None,
            CellValue::Text(s) => Some(BindValue::Text(s.clone())),
            CellValue::Bytes(b) => Some(BindValue::Bytes(b.clone())),
        }
    }
}
