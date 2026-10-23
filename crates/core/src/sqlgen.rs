//! Parameterized SQL statement generation from tracked changes.
//!
//! Faithful port of TablePro's `SQLStatementGenerator` + `DialectQuoteHelper`
//! + `SQLEscaping` + `RowMatchPolicy` + `AllDefaultsInsert`. Values are never
//! interpolated — only identifiers are quoted — so the write path cannot
//! inject SQL.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::change::{ChangeType, RowChange, RowID};
use crate::database_type::DatabaseType;
use crate::value::{BindValue, CellValue};

/// How placeholders are spelled for a prepared statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParameterStyle {
    QuestionMark,
    Dollar,
}

impl ParameterStyle {
    pub fn for_database_type(database_type: &DatabaseType) -> Self {
        match database_type.driver_id() {
            "postgresql" | "duckdb" => Self::Dollar,
            _ => Self::QuestionMark,
        }
    }

    pub fn placeholder(self, index: usize) -> String {
        match self {
            Self::Dollar => format!("${}", index + 1),
            Self::QuestionMark => "?".to_string(),
        }
    }
}

/// The engine facts statement generation needs: the identifier quote
/// character, whether string literals treat backslash as an escape
/// (MySQL family — writing ANSI rules there silently rewrites data), and
/// the placeholder style.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlDialect {
    pub identifier_quote: String,
    pub requires_backslash_escaping: bool,
}

impl SqlDialect {
    pub fn mysql() -> Self {
        Self { identifier_quote: "`".into(), requires_backslash_escaping: true }
    }

    pub fn ansi() -> Self {
        Self { identifier_quote: "\"".into(), requires_backslash_escaping: false }
    }

    pub fn mssql() -> Self {
        Self { identifier_quote: "[".into(), requires_backslash_escaping: false }
    }

    /// Dialect for a built-in engine. Unknown engines resolve to ANSI
    /// double quotes; a plugin could override this in a fuller port.
    pub fn for_database_type(database_type: &DatabaseType) -> Self {
        match database_type.driver_id() {
            "mysql" => Self::mysql(),
            "mssql" => Self::mssql(),
            _ => Self::ansi(),
        }
    }
}

/// Quote a SQL identifier with the dialect's quote character, doubling any
/// embedded quote. MSSQL brackets escape by doubling the closing bracket.
pub fn quote_identifier_from_dialect(dialect: &SqlDialect, name: &str) -> String {
    let q = dialect.identifier_quote.as_str();
    if q == "[" {
        format!("[{}]", name.replace(']', "]]"))
    } else {
        format!("{q}{}{q}", name.replace(q, &format!("{q}{q}")))
    }
}

/// Escape a string value for use in an ANSI SQL string literal: only doubles
/// single quotes and strips null bytes.
pub fn escape_string_literal(s: &str) -> String {
    s.replace('\'', "''").replace('\0', "")
}

/// Escape the body of a single-quoted literal the way backslash-escaping
/// engines (MySQL, MariaDB, ClickHouse, Snowflake) read it back. Order
/// matters — backslash first. `\Z` (0x1A, SUB) matters beyond tidiness: a
/// raw SUB byte truncates a dump fed to the Windows `mysql` client.
pub fn escape_string_literal_backslash(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\'', "''")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
        .replace('\0', "\\0")
        .replace('\u{08}', "\\b")
        .replace('\u{1A}', "\\Z")
}

/// Known SQL temporal function expressions that must not be quoted or
/// parameterized (canonical list from TablePro's `SQLEscaping`).
const TEMPORAL_FUNCTION_EXPRESSIONS: &[&str] = &[
    "NOW()",
    "CURRENT_TIMESTAMP()",
    "CURRENT_TIMESTAMP",
    "CURDATE()",
    "CURTIME()",
    "UTC_TIMESTAMP()",
    "UTC_DATE()",
    "UTC_TIME()",
    "LOCALTIME()",
    "LOCALTIME",
    "LOCALTIMESTAMP()",
    "LOCALTIMESTAMP",
    "SYSDATE()",
    "UNIX_TIMESTAMP()",
    "CURRENT_DATE()",
    "CURRENT_DATE",
    "CURRENT_TIME()",
    "CURRENT_TIME",
];

pub fn is_temporal_function(value: &str) -> bool {
    let trimmed = value.trim().to_uppercase();
    TEMPORAL_FUNCTION_EXPRESSIONS.contains(&trimmed.as_str())
}

/// The sentinel a cell value carries to mean "leave this column to the
/// server's default".
pub const DEFAULT_MARKER: &str = "__DEFAULT__";

/// How a table without a primary key identifies the row a save is about to
/// write: match every column against the value the grid read. The two sets
/// here are the columns that cannot take part in that plainly.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RowMatchPolicy {
    /// Columns left out of the match entirely, because the engine cannot
    /// compare the value at all.
    pub excluded_columns: BTreeSet<String>,
    /// Columns matched through the server's own text rendering (`CONCAT(col)`
    /// / `CAST(col AS NVARCHAR(MAX))`), because the text the grid read does
    /// not compare equal to what it was read from.
    pub text_columns: BTreeSet<String>,
}

impl RowMatchPolicy {
    pub fn none() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.excluded_columns.is_empty() && self.text_columns.is_empty()
    }

    /// What a keyless row match compares against for one column, already
    /// quoted. A keyed match never reaches here: it compares the primary
    /// key, which is the one thing the engine guarantees round-trips.
    pub fn match_expression(
        &self,
        column: &str,
        quoted: &str,
        value: &CellValue,
        database_type: &DatabaseType,
    ) -> String {
        if !self.text_columns.contains(column) {
            return quoted.to_string();
        }
        match database_type.driver_id() {
            "mssql" => {
                let target = if value.as_bytes().is_none() { "NVARCHAR(MAX)" } else { "VARBINARY(MAX)" };
                format!("CAST({quoted} AS {target})")
            }
            _ => format!("CONCAT({quoted})"),
        }
    }
}

/// The INSERT for a row whose every column the server fills in. Returning
/// nothing instead dropped the row from a save while the rest committed and
/// reported success — so this always has an answer where one exists.
pub fn all_defaults_insert(
    qualified_table: &str,
    database_type: &DatabaseType,
    first_writable_column: Option<&str>,
) -> Option<String> {
    match database_type.driver_id() {
        "postgresql" | "sqlite" => Some(format!("INSERT INTO {qualified_table} DEFAULT VALUES")),
        "mysql" => Some(format!("INSERT INTO {qualified_table} () VALUES ()")),
        _ => match database_type.id() {
            "mssql" => Some(format!("INSERT INTO {qualified_table} DEFAULT VALUES")),
            "databend" | "oracle" => first_writable_column
                .map(|col| format!("INSERT INTO {qualified_table} ({col}) VALUES (DEFAULT)")),
            _ => None,
        },
    }
}

/// A parameterized SQL statement with placeholders and bound values.
#[derive(Debug, Clone, PartialEq)]
pub struct ParameterizedStatement {
    pub sql: String,
    pub parameters: Vec<Option<BindValue>>,
}

/// What a statement is meant to do, so the executor can hold the server to
/// the row count the user asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowWriteKind {
    Update,
    Insert,
    Delete,
}

/// A statement plus the rows it is meant to touch, so a caller can hold the
/// server to that count and tell a change that became a statement from one
/// that did not.
#[derive(Debug, Clone)]
pub struct AttributedStatement {
    pub statement: ParameterizedStatement,
    pub kind: RowWriteKind,
    pub row_ids: Vec<RowID>,
}

impl AttributedStatement {
    pub fn row_count(&self) -> usize {
        self.row_ids.len()
    }
}

/// One column of a keyless/PK row match for DELETE.
struct DeleteColumnMatch {
    column: String,
    bound_value: Option<CellValue>,
}

/// Generates SQL statements from data changes.
pub struct SqlStatementGenerator {
    pub table_name: String,
    /// Written into every statement when set, so a generator can address a
    /// table outside whatever schema the connection happens to be on.
    pub schema_name: Option<String>,
    pub columns: Vec<String>,
    pub primary_key_columns: Vec<String>,
    /// Server-computed columns. They reject any written value, so they are
    /// dropped from every INSERT and UPDATE this generator produces.
    pub generated_columns: BTreeSet<String>,
    pub row_match_policy: RowMatchPolicy,
    pub database_type: DatabaseType,
    pub parameter_style: ParameterStyle,
    /// Set only by a restore that writes a row back with the identity it
    /// had: PostgreSQL refuses an explicit value for a `GENERATED ALWAYS`
    /// column unless the INSERT says so.
    pub insert_overrides_system_value: bool,
    dialect: SqlDialect,
}

impl SqlStatementGenerator {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        table_name: impl Into<String>,
        schema_name: Option<String>,
        columns: Vec<String>,
        primary_key_columns: Vec<String>,
        database_type: DatabaseType,
        generated_columns: BTreeSet<String>,
        row_match_policy: RowMatchPolicy,
        parameter_style: Option<ParameterStyle>,
        insert_overrides_system_value: bool,
        dialect: Option<SqlDialect>,
    ) -> Self {
        let resolved_dialect = dialect.unwrap_or_else(|| SqlDialect::for_database_type(&database_type));
        let resolved_style = parameter_style.unwrap_or_else(|| Self::default_parameter_style(&database_type));
        Self {
            table_name: table_name.into(),
            schema_name,
            columns,
            primary_key_columns,
            generated_columns,
            row_match_policy,
            database_type,
            parameter_style: resolved_style,
            insert_overrides_system_value,
            dialect: resolved_dialect,
        }
    }

    pub fn simple(
        table_name: &str,
        columns: Vec<String>,
        primary_key_columns: Vec<String>,
        database_type: DatabaseType,
    ) -> Self {
        Self::new(
            table_name,
            None,
            columns,
            primary_key_columns,
            database_type,
            BTreeSet::new(),
            RowMatchPolicy::none(),
            None,
            false,
            None,
        )
    }

    fn default_parameter_style(database_type: &DatabaseType) -> ParameterStyle {
        ParameterStyle::for_database_type(database_type)
    }

    pub fn quote_identifier(&self, name: &str) -> String {
        quote_identifier_from_dialect(&self.dialect, name)
    }

    /// The table as every statement spells it.
    pub fn qualified_table_name(&self) -> String {
        match &self.schema_name {
            Some(schema) if !schema.is_empty() => {
                format!("{}.{}", self.quote_identifier(schema), self.quote_identifier(&self.table_name))
            }
            _ => self.quote_identifier(&self.table_name),
        }
    }

    fn placeholder(&self, index: usize) -> String {
        self.parameter_style.placeholder(index)
    }

    pub fn max_bind_parameters(&self) -> usize {
        match self.database_type.driver_id() {
            "sqlite" => 32_766,
            "mssql" => 2_100,
            _ => 65_535,
        }
    }

    pub fn delete_all_rows_statement(&self) -> String {
        format!("DELETE FROM {}", self.qualified_table_name())
    }

    /// Generate all parameterized SQL statements from changes.
    #[allow(clippy::too_many_arguments)]
    pub fn generate_statements(
        &self,
        changes: &[RowChange],
        inserted_row_data: &BTreeMap<RowID, Vec<CellValue>>,
        deleted_row_ids: &BTreeSet<RowID>,
        inserted_row_ids: &BTreeSet<RowID>,
    ) -> Vec<ParameterizedStatement> {
        self.generate_attributed_statements(changes, inserted_row_data, deleted_row_ids, inserted_row_ids)
            .into_iter()
            .map(|a| a.statement)
            .collect()
    }

    /// The same statements, each carrying how many rows it is meant to
    /// touch.
    ///
    /// Emitted in the order the user made the changes, because that order
    /// can be load-bearing: a row deleted to free a unique value, and a new
    /// row taking that value, only work if the DELETE runs first. Deletes
    /// batch, but only across a consecutive run of them.
    #[allow(clippy::too_many_arguments)]
    pub fn generate_attributed_statements(
        &self,
        changes: &[RowChange],
        inserted_row_data: &BTreeMap<RowID, Vec<CellValue>>,
        deleted_row_ids: &BTreeSet<RowID>,
        inserted_row_ids: &BTreeSet<RowID>,
    ) -> Vec<AttributedStatement> {
        let mut statements: Vec<AttributedStatement> = Vec::new();
        let mut delete_run: Vec<&RowChange> = Vec::new();

        let mut sorted: Vec<&RowChange> = changes.iter().collect();
        sorted.sort_by_key(|c| c.sequence);

        for change in sorted {
            match change.change_type {
                ChangeType::Update => {
                    Self::flush_delete_run(&mut statements, &mut delete_run, self);
                    if let Some(stmt) = self.generate_update_sql(change) {
                        statements.push(AttributedStatement {
                            statement: stmt,
                            kind: RowWriteKind::Update,
                            row_ids: vec![change.row_id],
                        });
                    }
                }
                ChangeType::Insert => {
                    if !inserted_row_ids.contains(&change.row_id) {
                        continue;
                    }
                    Self::flush_delete_run(&mut statements, &mut delete_run, self);
                    if let Some(stmt) = self.generate_insert_sql(change, inserted_row_data) {
                        statements.push(AttributedStatement {
                            statement: stmt,
                            kind: RowWriteKind::Insert,
                            row_ids: vec![change.row_id],
                        });
                    }
                }
                ChangeType::Delete => {
                    if !deleted_row_ids.contains(&change.row_id) {
                        continue;
                    }
                    delete_run.push(change);
                }
            }
        }
        Self::flush_delete_run(&mut statements, &mut delete_run, self);

        statements
    }

    fn flush_delete_run(
        statements: &mut Vec<AttributedStatement>,
        delete_run: &mut Vec<&RowChange>,
        generator: &Self,
    ) {
        if delete_run.is_empty() {
            return;
        }
        let run: Vec<RowChange> = delete_run.iter().map(|c| (*c).clone()).collect();
        statements.extend(generator.generate_delete_statements(&run));
        delete_run.clear();
    }

    // ---- INSERT ----------------------------------------------------------

    fn generate_insert_sql(
        &self,
        change: &RowChange,
        inserted_row_data: &BTreeMap<RowID, Vec<CellValue>>,
    ) -> Option<ParameterizedStatement> {
        if let Some(values) = inserted_row_data.get(&change.row_id) {
            return self.generate_insert_sql_from_stored_data(values);
        }
        self.generate_insert_sql_from_cell_changes(change)
    }

    fn generate_insert_sql_from_stored_data(&self, values: &[CellValue]) -> Option<ParameterizedStatement> {
        let mut non_default_columns: Vec<String> = Vec::new();
        let mut placeholder_parts: Vec<String> = Vec::new();
        let mut bind_parameters: Vec<Option<BindValue>> = Vec::new();

        for (index, value) in values.iter().enumerate() {
            if index >= self.columns.len() {
                continue;
            }
            // `DEFAULT_MARKER` in stored inserted-row data is the app-created
            // "let the server fill this column" sentinel ("+ Row" seeds it),
            // never user text: user edits replace it before generation. It is
            // skipped so untouched columns fall back to server defaults.
            if matches!(value, CellValue::Text(s) if s == DEFAULT_MARKER) {
                continue;
            }
            let column_name = &self.columns[index];
            if self.generated_columns.contains(column_name) {
                continue;
            }

            non_default_columns.push(self.quote_identifier(column_name));

            bind_parameters.push(Option::<BindValue>::from(value));
            placeholder_parts.push(self.placeholder(bind_parameters.len() - 1));
        }

        if non_default_columns.is_empty() {
            return self.all_defaults_insert_statement();
        }

        let column_list = non_default_columns.join(", ");
        let placeholders = placeholder_parts.join(", ");
        let system_value_override = if self.insert_overrides_system_value { " OVERRIDING SYSTEM VALUE" } else { "" };
        let sql = format!(
            "INSERT INTO {} ({}){} VALUES ({})",
            self.qualified_table_name(),
            column_list,
            system_value_override,
            placeholders
        );

        Some(ParameterizedStatement { sql, parameters: bind_parameters })
    }

    fn all_defaults_insert_statement(&self) -> Option<ParameterizedStatement> {
        let first_writable = self
            .columns
            .iter()
            .find(|c| !self.generated_columns.contains(*c))
            .map(|c| self.quote_identifier(c));
        all_defaults_insert(
            &self.qualified_table_name(),
            &self.database_type,
            first_writable.as_deref(),
        )
        .map(|sql| ParameterizedStatement { sql, parameters: Vec::new() })
    }

    /// Insert one row from explicit columns/values (import path).
    pub fn insert_statement(
        &self,
        insert_columns: &[String],
        values: &[CellValue],
    ) -> Option<ParameterizedStatement> {
        if insert_columns.is_empty() || insert_columns.len() != values.len() {
            return None;
        }

        let mut bind_parameters: Vec<Option<BindValue>> = Vec::new();
        let column_list: Vec<String> = insert_columns.iter().map(|c| self.quote_identifier(c)).collect();
        let mut placeholders: Vec<String> = Vec::new();
        for value in values {
            bind_parameters.push(Option::<BindValue>::from(value));
            placeholders.push(self.placeholder(bind_parameters.len() - 1));
        }

        Some(ParameterizedStatement {
            sql: format!(
                "INSERT INTO {} ({}) VALUES ({})",
                self.qualified_table_name(),
                column_list.join(", "),
                placeholders.join(", ")
            ),
            parameters: bind_parameters,
        })
    }

    /// Multi-row INSERT: `INSERT INTO t (c1, c2) VALUES (?, ?), (?, ?)`.
    pub fn insert_statement_multi_row(
        &self,
        insert_columns: &[String],
        rows: &[Vec<CellValue>],
    ) -> Option<ParameterizedStatement> {
        if insert_columns.is_empty() || rows.is_empty() || !rows.iter().all(|r| r.len() == insert_columns.len()) {
            return None;
        }

        let mut bind_parameters: Vec<Option<BindValue>> = Vec::new();
        let column_list: Vec<String> = insert_columns.iter().map(|c| self.quote_identifier(c)).collect();
        let row_tuples: Vec<String> = rows
            .iter()
            .map(|values| {
                let mut placeholders: Vec<String> = Vec::new();
                for value in values {
                    bind_parameters.push(Option::<BindValue>::from(value));
                    placeholders.push(self.placeholder(bind_parameters.len() - 1));
                }
                format!("({})", placeholders.join(", "))
            })
            .collect();

        Some(ParameterizedStatement {
            sql: format!(
                "INSERT INTO {} ({}) VALUES {}",
                self.qualified_table_name(),
                column_list.join(", "),
                row_tuples.join(", ")
            ),
            parameters: bind_parameters,
        })
    }

    fn generate_insert_sql_from_cell_changes(&self, change: &RowChange) -> Option<ParameterizedStatement> {
        if change.cell_changes.is_empty() {
            return None;
        }

        let non_default_changes: Vec<&crate::change::CellChange> = change
            .cell_changes
            .iter()
            .filter(|c| !self.generated_columns.contains(&c.column_name))
            .collect();

        if non_default_changes.is_empty() {
            return None;
        }

        let column_names: Vec<String> = non_default_changes
            .iter()
            .map(|c| self.quote_identifier(&c.column_name))
            .collect();

        let mut parameters: Vec<Option<BindValue>> = Vec::new();
        let mut placeholders: Vec<String> = Vec::new();
        for cell_change in &non_default_changes {
            parameters.push(Option::<BindValue>::from(&cell_change.new_value));
            placeholders.push(self.placeholder(parameters.len() - 1));
        }

        Some(ParameterizedStatement {
            sql: format!(
                "INSERT INTO {} ({}) VALUES ({})",
                self.qualified_table_name(),
                column_names.join(", "),
                placeholders.join(", ")
            ),
            parameters,
        })
    }

    // ---- UPDATE ----------------------------------------------------------

    pub fn generate_update_sql(&self, change: &RowChange) -> Option<ParameterizedStatement> {
        if change.cell_changes.is_empty() {
            return None;
        }

        let writable_changes: Vec<&crate::change::CellChange> = change
            .cell_changes
            .iter()
            .filter(|c| !self.generated_columns.contains(&c.column_name))
            .collect();
        if writable_changes.is_empty() {
            return None;
        }

        let mut parameters: Vec<Option<BindValue>> = Vec::new();
        let mut set_clauses: Vec<String> = Vec::new();
        for cell_change in &writable_changes {
            let quoted = self.quote_identifier(&cell_change.column_name);
            parameters.push(Option::<BindValue>::from(&cell_change.new_value));
            set_clauses.push(format!("{quoted} = {}", self.placeholder(parameters.len() - 1)));
        }
        let set_clauses = set_clauses.join(", ");

        if !self.primary_key_columns.is_empty() {
            let mut conditions: Vec<String> = Vec::new();

            for pk_column in &self.primary_key_columns {
                let Some(pk_column_index) = self.columns.iter().position(|c| c == pk_column) else {
                    return None;
                };

                let pk_value = change
                    .original_row
                    .as_ref()
                    .and_then(|row| row.get(pk_column_index))
                    .cloned()
                    .or_else(|| {
                        change
                            .cell_changes
                            .iter()
                            .find(|c| c.column_name == *pk_column)
                            .map(|c| c.old_value.clone())
                    });

                let Some(pk_value) = pk_value else {
                    return None;
                };
                if pk_value.is_null() {
                    return None;
                }

                parameters.push(Option::<BindValue>::from(&pk_value));
                conditions.push(format!(
                    "{} = {}",
                    self.quote_identifier(pk_column),
                    self.placeholder(parameters.len() - 1)
                ));
            }

            if conditions.is_empty() {
                return None;
            }

            let where_clause = conditions.join(" AND ");
            return Some(ParameterizedStatement {
                sql: format!(
                    "UPDATE {} SET {} WHERE {}",
                    self.qualified_table_name(),
                    set_clauses,
                    where_clause
                ),
                parameters,
            });
        }

        // Keyless: match every column against the row as it was read.
        let Some(original_row) = &change.original_row else {
            return None;
        };

        let mut conditions: Vec<String> = Vec::new();
        for (index, column_name) in self.columns.iter().enumerate() {
            if index >= original_row.len() || self.row_match_policy.excluded_columns.contains(column_name) {
                continue;
            }
            let value = &original_row[index];
            let matched = self.row_match_policy.match_expression(
                column_name,
                &self.quote_identifier(column_name),
                value,
                &self.database_type,
            );
            if value.is_null() {
                conditions.push(format!("{matched} IS NULL"));
            } else {
                parameters.push(Option::<BindValue>::from(value));
                conditions.push(format!("{matched} = {}", self.placeholder(parameters.len() - 1)));
            }
        }

        if conditions.is_empty() {
            return None;
        }

        let where_clause = conditions.join(" AND ");
        Some(ParameterizedStatement {
            sql: format!(
                "UPDATE {} SET {} WHERE {}",
                self.qualified_table_name(),
                set_clauses,
                where_clause
            ),
            parameters,
        })
    }

    // ---- DELETE ----------------------------------------------------------

    fn generate_delete_statements(&self, changes: &[RowChange]) -> Vec<AttributedStatement> {
        let row_matches: Vec<(RowID, Vec<DeleteColumnMatch>)> = changes
            .iter()
            .filter_map(|change| {
                self.delete_row_matches(change).map(|matches| (change.row_id, matches))
            })
            .collect();
        if row_matches.is_empty() {
            return Vec::new();
        }

        let mut statements: Vec<AttributedStatement> = Vec::new();
        let mut chunk: Vec<(RowID, Vec<DeleteColumnMatch>)> = Vec::new();
        let mut chunk_parameter_count = 0usize;

        let flush = |chunk: &mut Vec<(RowID, Vec<DeleteColumnMatch>)>,
                     statements: &mut Vec<AttributedStatement>| {
            if chunk.is_empty() {
                return;
            }
            let mut parameters: Vec<Option<BindValue>> = Vec::new();
            let row_clauses: Vec<String> = chunk
                .iter()
                .map(|(_row_id, matches)| {
                    let mut conditions: Vec<String> = Vec::new();
                    for m in matches {
                        let value = m.bound_value.clone().unwrap_or(CellValue::Null);
                        let matched = self.row_match_policy.match_expression(
                            &m.column,
                            &self.quote_identifier(&m.column),
                            &value,
                            &self.database_type,
                        );
                        match &m.bound_value {
                            None => conditions.push(format!("{matched} IS NULL")),
                            Some(v) => {
                                parameters.push(Option::<BindValue>::from(v));
                                conditions
                                    .push(format!("{matched} = {}", self.placeholder(parameters.len() - 1)));
                            }
                        }
                    }
                    let joined = conditions.join(" AND ");
                    if matches.len() > 1 {
                        format!("({joined})")
                    } else {
                        joined
                    }
                })
                .collect();

            let where_clause = row_clauses.join(" OR ");
            let row_ids: Vec<RowID> = chunk.iter().map(|(id, _)| *id).collect();
            statements.push(AttributedStatement {
                statement: ParameterizedStatement {
                    sql: format!("DELETE FROM {} WHERE {}", self.qualified_table_name(), where_clause),
                    parameters,
                },
                kind: RowWriteKind::Delete,
                row_ids,
            });
            chunk.clear();
        };

        // Keyless deletes with an exclusion policy must never be OR-batched:
        // a wrong OR match deletes unrelated duplicate rows.
        let matches_one_row_per_statement =
            self.primary_key_columns.is_empty() && !self.row_match_policy.excluded_columns.is_empty();

        for (row_id, matches) in row_matches {
            let row_parameter_count = matches.iter().filter(|m| m.bound_value.is_some()).count();
            if !chunk.is_empty()
                && (matches_one_row_per_statement
                    || chunk_parameter_count + row_parameter_count > self.max_bind_parameters())
            {
                flush(&mut chunk, &mut statements);
                chunk_parameter_count = 0;
            }
            chunk.push((row_id, matches));
            chunk_parameter_count += row_parameter_count;
        }

        flush(&mut chunk, &mut statements);

        statements
    }

    fn delete_row_matches(&self, change: &RowChange) -> Option<Vec<DeleteColumnMatch>> {
        let original_row = change.original_row.as_ref()?;

        if !self.primary_key_columns.is_empty() {
            let mut matches: Vec<DeleteColumnMatch> = Vec::new();
            for pk_column in &self.primary_key_columns {
                let pk_index = self.columns.iter().position(|c| c == pk_column)?;
                if pk_index >= original_row.len() {
                    return None;
                }
                let value = &original_row[pk_index];
                if value.is_null() {
                    return None;
                }
                matches.push(DeleteColumnMatch {
                    column: pk_column.clone(),
                    bound_value: Some(value.clone()),
                });
            }
            return if matches.is_empty() { None } else { Some(matches) };
        }

        let mut matches: Vec<DeleteColumnMatch> = Vec::new();
        for (index, column_name) in self.columns.iter().enumerate() {
            if index >= original_row.len() || self.row_match_policy.excluded_columns.contains(column_name) {
                continue;
            }
            let value = &original_row[index];
            matches.push(DeleteColumnMatch {
                column: column_name.clone(),
                bound_value: if value.is_null() { None } else { Some(value.clone()) },
            });
        }
        if matches.is_empty() { None } else { Some(matches) }
    }
}
