//! Values used by result grids, parameter binding, and explicit SQL writes.
//!
//! [`CellValue`] stays deliberately data-only: text is always text, even when
//! it happens to look like `NOW()`, `CURRENT_DATE`, or `__DEFAULT__`. Callers
//! that intentionally want server-side syntax must opt into [`SqlValue`].

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
        s.map(|v| CellValue::Text(v.to_string()))
            .unwrap_or(CellValue::Null)
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindValue {
    Text(String),
    Bytes(Vec<u8>),
}

impl From<String> for BindValue {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}

impl From<&str> for BindValue {
    fn from(value: &str) -> Self {
        Self::Text(value.to_string())
    }
}

impl From<Vec<u8>> for BindValue {
    fn from(value: Vec<u8>) -> Self {
        Self::Bytes(value)
    }
}

impl From<&[u8]> for BindValue {
    fn from(value: &[u8]) -> Self {
        Self::Bytes(value.to_vec())
    }
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

/// A validated SQL fragment used only when a caller deliberately asks the
/// generator to emit an expression instead of binding a literal value.
///
/// This is intentionally distinct from [`CellValue::Text`]. The conservative
/// validation rejects statement separators, comments, NULs, unbalanced
/// parentheses, and unterminated quotes. It is not a SQL parser; callers must
/// still treat expressions as trusted application input rather than raw user
/// text.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SqlExpression(String);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SqlExpressionError {
    #[error("SQL expression cannot be empty")]
    Empty,
    #[error("SQL expression contains a statement separator, comment, or NUL")]
    UnsafeToken,
    #[error("SQL expression has unbalanced parentheses or an unterminated quote")]
    Unbalanced,
}

impl SqlExpression {
    pub fn new(sql: impl Into<String>) -> Result<Self, SqlExpressionError> {
        let sql = sql.into();
        let trimmed = sql.trim();
        if trimmed.is_empty() {
            return Err(SqlExpressionError::Empty);
        }
        if trimmed.contains('\0')
            || trimmed.contains(';')
            || trimmed.contains("--")
            || trimmed.contains("/*")
            || trimmed.contains("*/")
        {
            return Err(SqlExpressionError::UnsafeToken);
        }
        if !expression_is_balanced(trimmed) {
            return Err(SqlExpressionError::Unbalanced);
        }
        Ok(Self(trimmed.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl AsRef<str> for SqlExpression {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl std::fmt::Display for SqlExpression {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An explicit input to SQL generation.
///
/// Literal values remain parameters. `Default` and `Expression` are emitted as
/// SQL only because the caller selected those variants explicitly.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "lowercase")]
pub enum SqlValue {
    Literal(CellValue),
    Default,
    Expression(SqlExpression),
}

impl SqlValue {
    pub fn literal(value: impl Into<CellValue>) -> Self {
        Self::Literal(value.into())
    }

    pub fn text(value: impl Into<String>) -> Self {
        Self::Literal(CellValue::Text(value.into()))
    }

    pub fn bytes(value: impl Into<Vec<u8>>) -> Self {
        Self::Literal(CellValue::Bytes(value.into()))
    }

    pub fn null() -> Self {
        Self::Literal(CellValue::Null)
    }

    pub fn expression(sql: impl Into<String>) -> Result<Self, SqlExpressionError> {
        SqlExpression::new(sql).map(Self::Expression)
    }
}

impl From<CellValue> for SqlValue {
    fn from(value: CellValue) -> Self {
        Self::Literal(value)
    }
}

impl From<&CellValue> for SqlValue {
    fn from(value: &CellValue) -> Self {
        Self::Literal(value.clone())
    }
}

impl From<SqlExpression> for SqlValue {
    fn from(value: SqlExpression) -> Self {
        Self::Expression(value)
    }
}

fn expression_is_balanced(sql: &str) -> bool {
    let bytes = sql.as_bytes();
    let mut index = 0;
    let mut depth = 0usize;

    while index < bytes.len() {
        match bytes[index] {
            b'\'' | b'"' | b'`' => {
                let quote = bytes[index];
                index += 1;
                let mut closed = false;
                while index < bytes.len() {
                    if bytes[index] == b'\\' {
                        index = (index + 2).min(bytes.len());
                        continue;
                    }
                    if bytes[index] == quote {
                        if index + 1 < bytes.len() && bytes[index + 1] == quote {
                            index += 2;
                            continue;
                        }
                        index += 1;
                        closed = true;
                        break;
                    }
                    index += 1;
                }
                if !closed {
                    return false;
                }
            }
            b'[' => {
                index += 1;
                let mut closed = false;
                while index < bytes.len() {
                    if bytes[index] == b']' {
                        if index + 1 < bytes.len() && bytes[index + 1] == b']' {
                            index += 2;
                            continue;
                        }
                        index += 1;
                        closed = true;
                        break;
                    }
                    index += 1;
                }
                if !closed {
                    return false;
                }
            }
            b'(' => {
                depth += 1;
                index += 1;
            }
            b')' => {
                if depth == 0 {
                    return false;
                }
                depth -= 1;
                index += 1;
            }
            _ => index += 1,
        }
    }

    depth == 0
}
