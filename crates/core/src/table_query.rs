//! Safe, dialect-aware `SELECT` generation for table browsing.

use std::fmt;

use crate::database_type::DatabaseType;
use crate::sqlgen::{quote_identifier_from_dialect, ParameterStyle, ParameterizedStatement, SqlDialect};
use crate::value::{BindValue, CellValue};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SortColumn {
    pub column: String,
    pub direction: SortDirection,
}

impl SortColumn {
    pub fn new(column: impl Into<String>, direction: SortDirection) -> Self {
        Self { column: column.into(), direction }
    }

    pub fn ascending(column: impl Into<String>) -> Self {
        Self::new(column, SortDirection::Ascending)
    }

    pub fn descending(column: impl Into<String>) -> Self {
        Self::new(column, SortDirection::Descending)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDirection {
    Ascending,
    Descending,
}

impl SortDirection {
    fn sql(self) -> &'static str {
        match self {
            Self::Ascending => "ASC",
            Self::Descending => "DESC",
        }
    }
}

/// Operators supported by the table filter UI. Every value-bearing operator
/// uses a bind parameter; identifiers are always dialect-quoted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterOperator {
    Equal,
    NotEqual,
    LessThan,
    LessThanOrEqual,
    GreaterThan,
    GreaterThanOrEqual,
    Contains,
    NotContains,
    StartsWith,
    EndsWith,
    IsNull,
    IsNotNull,
}

impl FilterOperator {
    pub fn requires_value(self) -> bool {
        !matches!(self, Self::IsNull | Self::IsNotNull)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilterCondition {
    pub column: String,
    pub operator: FilterOperator,
    pub value: Option<CellValue>,
    pub enabled: bool,
}

impl FilterCondition {
    pub fn new(
        column: impl Into<String>,
        operator: FilterOperator,
        value: impl Into<CellValue>,
    ) -> Self {
        Self {
            column: column.into(),
            operator,
            value: Some(value.into()),
            enabled: true,
        }
    }

    pub fn null(column: impl Into<String>, is_null: bool) -> Self {
        Self {
            column: column.into(),
            operator: if is_null { FilterOperator::IsNull } else { FilterOperator::IsNotNull },
            value: None,
            enabled: true,
        }
    }

    pub fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TableQueryError {
    #[error("table name cannot be empty")]
    EmptyTable,
    #[error("column name cannot be empty")]
    EmptyColumn,
    #[error("filter operator {operator:?} requires a value for column {column:?}")]
    MissingFilterValue { column: String, operator: FilterOperator },
    #[error("filter operator {operator:?} does not accept a value for column {column:?}")]
    UnexpectedFilterValue { column: String, operator: FilterOperator },
    #[error("filter operator {operator:?} requires a text value for column {column:?}")]
    NonTextPatternValue { column: String, operator: FilterOperator },
}

/// Builds the query used to browse or reload a table.
///
/// The builder owns only SQL structure. It never accepts an arbitrary `WHERE`
/// fragment, so user-provided values cannot become executable SQL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableQuery {
    pub table_name: String,
    pub schema_name: Option<String>,
    pub columns: Vec<String>,
    pub filters: Vec<FilterCondition>,
    pub sort: Vec<SortColumn>,
    pub limit: Option<u64>,
    pub offset: Option<u64>,
    pub parameter_style: ParameterStyle,
    dialect: SqlDialect,
}

impl TableQuery {
    pub fn new(table_name: impl Into<String>, database_type: &DatabaseType) -> Self {
        Self {
            table_name: table_name.into(),
            schema_name: None,
            columns: Vec::new(),
            filters: Vec::new(),
            sort: Vec::new(),
            limit: None,
            offset: None,
            parameter_style: ParameterStyle::for_database_type(database_type),
            dialect: SqlDialect::for_database_type(database_type),
        }
    }

    pub fn with_dialect(
        table_name: impl Into<String>,
        dialect: SqlDialect,
        parameter_style: ParameterStyle,
    ) -> Self {
        Self {
            table_name: table_name.into(),
            schema_name: None,
            columns: Vec::new(),
            filters: Vec::new(),
            sort: Vec::new(),
            limit: None,
            offset: None,
            parameter_style,
            dialect,
        }
    }

    pub fn schema(mut self, schema: impl Into<String>) -> Self {
        self.schema_name = Some(schema.into());
        self
    }

    pub fn columns<I, S>(mut self, columns: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.columns = columns.into_iter().map(Into::into).collect();
        self
    }

    pub fn filter(mut self, condition: FilterCondition) -> Self {
        self.filters.push(condition);
        self
    }

    pub fn filters<I>(mut self, conditions: I) -> Self
    where
        I: IntoIterator<Item = FilterCondition>,
    {
        self.filters.extend(conditions);
        self
    }

    pub fn sort_by(mut self, column: impl Into<String>, direction: SortDirection) -> Self {
        self.sort.push(SortColumn::new(column, direction));
        self
    }

    pub fn limit(mut self, limit: u64) -> Self {
        self.limit = Some(limit);
        self
    }

    pub fn offset(mut self, offset: u64) -> Self {
        self.offset = Some(offset);
        self
    }

    pub fn build(&self) -> Result<ParameterizedStatement, TableQueryError> {
        self.validate_identifier(&self.table_name, true)?;
        if let Some(schema) = &self.schema_name {
            self.validate_identifier(schema, false)?;
        }
        for column in &self.columns {
            self.validate_identifier(column, false)?;
        }
        for sort in &self.sort {
            self.validate_identifier(&sort.column, false)?;
        }

        let projection = if self.columns.is_empty() {
            "*".to_string()
        } else {
            self.columns
                .iter()
                .map(|column| self.quote_identifier(column))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let mut sql = format!("SELECT {projection} FROM {}", self.qualified_table_name());
        let mut parameters = Vec::new();
        let mut clauses = Vec::new();

        for condition in self.filters.iter().filter(|condition| condition.enabled) {
            self.validate_identifier(&condition.column, false)?;
            clauses.push(self.build_filter(condition, &mut parameters)?);
        }
        if !clauses.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&clauses.join(" AND "));
        }

        if !self.sort.is_empty() {
            sql.push_str(" ORDER BY ");
            sql.push_str(
                &self
                    .sort
                    .iter()
                    .map(|sort| {
                        format!("{} {}", self.quote_identifier(&sort.column), sort.direction.sql())
                    })
                    .collect::<Vec<_>>()
                    .join(", "),
            );
        }

        if let Some(limit) = self.limit {
            sql.push_str(&format!(" LIMIT {limit}"));
        }
        if let Some(offset) = self.offset {
            // Every supported browse dialect accepts OFFSET after LIMIT. The
            // unlimited forms preserve offset-only pagination portably across
            // the built-in SQLite/MySQL/PostgreSQL families.
            if self.limit.is_none() {
                if self.dialect.identifier_quote == "`" {
                    sql.push_str(" LIMIT 18446744073709551615");
                } else {
                    sql.push_str(" LIMIT ALL");
                }
            }
            sql.push_str(&format!(" OFFSET {offset}"));
        }

        Ok(ParameterizedStatement { sql, parameters })
    }

    fn build_filter(
        &self,
        condition: &FilterCondition,
        parameters: &mut Vec<Option<BindValue>>,
    ) -> Result<String, TableQueryError> {
        let column = self.quote_identifier(&condition.column);
        if !condition.operator.requires_value() {
            if condition.value.is_some() {
                return Err(TableQueryError::UnexpectedFilterValue {
                    column: condition.column.clone(),
                    operator: condition.operator,
                });
            }
            return Ok(match condition.operator {
                FilterOperator::IsNull => format!("{column} IS NULL"),
                FilterOperator::IsNotNull => format!("{column} IS NOT NULL"),
                _ => unreachable!("value-bearing operators returned above"),
            });
        }

        let value = condition
            .value
            .as_ref()
            .ok_or_else(|| TableQueryError::MissingFilterValue {
                column: condition.column.clone(),
                operator: condition.operator,
            })?;

        let (operator, bind_value) = match condition.operator {
            FilterOperator::Equal => ("=", value.clone()),
            FilterOperator::NotEqual => ("<>", value.clone()),
            FilterOperator::LessThan => ("<", value.clone()),
            FilterOperator::LessThanOrEqual => ("<=", value.clone()),
            FilterOperator::GreaterThan => (">", value.clone()),
            FilterOperator::GreaterThanOrEqual => (">=", value.clone()),
            FilterOperator::Contains
            | FilterOperator::NotContains
            | FilterOperator::StartsWith
            | FilterOperator::EndsWith => {
                let Some(text) = value.as_text() else {
                    return Err(TableQueryError::NonTextPatternValue {
                        column: condition.column.clone(),
                        operator: condition.operator,
                    });
                };
                let escaped = escape_like_pattern(text);
                let pattern = match condition.operator {
                    FilterOperator::Contains | FilterOperator::NotContains => {
                        format!("%{escaped}%")
                    }
                    FilterOperator::StartsWith => format!("{escaped}%"),
                    FilterOperator::EndsWith => format!("%{escaped}"),
                    _ => unreachable!(),
                };
                let operator = if condition.operator == FilterOperator::NotContains {
                    "NOT LIKE"
                } else {
                    "LIKE"
                };
                (operator, CellValue::Text(pattern))
            }
            FilterOperator::IsNull | FilterOperator::IsNotNull => unreachable!(),
        };

        parameters.push(Option::<BindValue>::from(&bind_value));
        let placeholder = self.placeholder(parameters.len() - 1);
        let escape = if matches!(
            condition.operator,
            FilterOperator::Contains
                | FilterOperator::NotContains
                | FilterOperator::StartsWith
                | FilterOperator::EndsWith
        ) {
            " ESCAPE '\\'"
        } else {
            ""
        };
        Ok(format!("{column} {operator} {placeholder}{escape}"))
    }

    fn validate_identifier(&self, identifier: &str, table: bool) -> Result<(), TableQueryError> {
        if identifier.is_empty() {
            if table {
                Err(TableQueryError::EmptyTable)
            } else {
                Err(TableQueryError::EmptyColumn)
            }
        } else {
            Ok(())
        }
    }

    fn quote_identifier(&self, identifier: &str) -> String {
        quote_identifier_from_dialect(&self.dialect, identifier)
    }

    fn qualified_table_name(&self) -> String {
        match self.schema_name.as_deref() {
            Some(schema) if !schema.is_empty() => format!(
                "{}.{}",
                self.quote_identifier(schema),
                self.quote_identifier(&self.table_name)
            ),
            _ => self.quote_identifier(&self.table_name),
        }
    }

    fn placeholder(&self, index: usize) -> String {
        self.parameter_style.placeholder(index)
    }
}

fn escape_like_pattern(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

impl From<String> for CellValue {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}

impl From<&str> for CellValue {
    fn from(value: &str) -> Self {
        Self::Text(value.to_string())
    }
}

impl fmt::Display for SortDirection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.sql())
    }
}
