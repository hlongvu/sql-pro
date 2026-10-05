//! SQLite driver over rusqlite, run on the shared Tokio blocking pool.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sqlpro_core::value::CellValue;
use sqlpro_core::connection::ConnectionConfig;
use sqlpro_core::driver::DatabaseDriver;
use sqlpro_core::error::{Error, Result};
use sqlpro_core::result::QueryResult;
use sqlpro_core::schema::{ColumnInfo, TableInfo};

use crate::runtime::on_blocking;
use crate::sql_split::split_statements;

pub struct SqliteDriver {
    conn: Arc<Mutex<rusqlite::Connection>>,
}

impl SqliteDriver {
    pub fn connect(config: &ConnectionConfig) -> Result<Self> {
        let path = config
            .path
            .as_deref()
            .ok_or_else(|| Error::Connection("SQLite connection has no path".into()))?;
        let conn = if path == ":memory:" {
            rusqlite::Connection::open_in_memory()
        } else {
            rusqlite::Connection::open_with_flags(
                path,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
                    | rusqlite::OpenFlags::SQLITE_OPEN_CREATE
                    | rusqlite::OpenFlags::SQLITE_OPEN_URI,
            )
        }
        .map_err(|e| Error::Connection(e.to_string()))?;
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }
}

#[async_trait::async_trait]
impl DatabaseDriver for SqliteDriver {
    async fn ping(&self) -> Result<()> {
        self.execute("SELECT 1").await.map(|_| ())
    }

    async fn execute(&self, sql: &str) -> Result<QueryResult> {
        let conn = Arc::clone(&self.conn);
        let sql = sql.to_string();
        on_blocking(move || run_script(&conn, &sql)).await
    }

    async fn list_tables(&self) -> Result<Vec<TableInfo>> {
        let result = self
            .execute(
                "SELECT name, type FROM sqlite_master \
                 WHERE name NOT LIKE 'sqlite_%' ORDER BY type, name",
            )
            .await?;
        let mut tables = Vec::new();
        for row in &result.rows {
            let name = match row.first() {
                Some(CellValue::Text(s)) => s.clone(),
                _ => continue,
            };
            let is_view = matches!(row.get(1), Some(CellValue::Text(t)) if t == "view");
            tables.push(TableInfo { name, is_view, row_estimate: None });
        }
        Ok(tables)
    }

    async fn list_columns(&self, table: &str) -> Result<Vec<ColumnInfo>> {
        // quote() safely wraps the identifier for the table-valued pragma.
        let result = self
            .execute(&format!(
                "SELECT name, type, \"notnull\", pk FROM pragma_table_info({})",
                quote_string(table)
            ))
            .await?;
        let mut columns = Vec::new();
        for row in &result.rows {
            let name = match row.first() {
                Some(CellValue::Text(s)) => s.clone(),
                _ => continue,
            };
            let data_type = match row.get(1) {
                Some(CellValue::Text(s)) => s.clone(),
                _ => String::new(),
            };
            let notnull = matches!(row.get(2), Some(CellValue::Text(s)) if s == "1");
            let pk = matches!(row.get(3), Some(CellValue::Text(s)) if s == "1");
            columns.push(ColumnInfo {
                name,
                data_type,
                nullable: !notnull,
                is_primary_key: pk,
            });
        }
        Ok(columns)
    }

    fn label(&self) -> String {
        "SQLite".to_string()
    }
}

/// Run a script (possibly multiple statements) on one connection,
/// accumulating affected rows and keeping the last row-returning
/// statement's result.
fn run_script(conn: &Mutex<rusqlite::Connection>, sql: &str) -> Result<QueryResult> {
    let statements = split_statements(sql);
    if statements.is_empty() {
        return Ok(QueryResult::empty(Duration::ZERO));
    }

    let conn = conn
        .lock()
        .map_err(|_| Error::Driver("poisoned connection".into()))?;
    let started = Instant::now();
    let mut total_affected: u64 = 0;
    let mut last_rows: Option<(Vec<ColumnInfo>, Vec<Vec<CellValue>>)> = None;

    for stmt in &statements {
        let mut prepared = conn
            .prepare(stmt)
            .map_err(|e| Error::Query(format!("{e} (in: {stmt})")))?;
        let column_names: Vec<String> =
            prepared.column_names().iter().map(|s| s.to_string()).collect();
        let column_count = prepared.column_count();

        if column_count > 0 {
            let mut rows_out = Vec::new();
            let mut rows = prepared.query([]).map_err(|e| Error::Query(e.to_string()))?;
            while let Some(row) = rows.next().map_err(|e| Error::Query(e.to_string()))? {
                let mut out = Vec::with_capacity(column_count);
                for i in 0..column_count {
                    out.push(value_ref_to_cell(
                        row.get_ref(i).map_err(|e| Error::Query(e.to_string()))?,
                    ));
                }
                rows_out.push(out);
            }
            last_rows = Some((columns_from_names(&column_names), rows_out));
        } else {
            total_affected +=
                prepared.execute([]).map_err(|e| Error::Query(e.to_string()))? as u64;
        }
    }

    let duration = started.elapsed();
    match last_rows {
        Some((columns, rows)) => Ok(QueryResult {
            columns,
            rows,
            rows_affected: total_affected,
            duration,
            is_query: true,
        }),
        None => Ok(QueryResult {
            columns: Vec::new(),
            rows: Vec::new(),
            rows_affected: total_affected,
            duration,
            is_query: false,
        }),
    }
}

fn columns_from_names(names: &[String]) -> Vec<ColumnInfo> {
    names
        .iter()
        .map(|name| ColumnInfo {
            name: name.clone(),
            data_type: String::new(),
            nullable: true,
            is_primary_key: false,
        })
        .collect()
}

fn value_ref_to_cell(v: rusqlite::types::ValueRef<'_>) -> CellValue {
    use rusqlite::types::ValueRef;
    match v {
        ValueRef::Null => CellValue::Null,
        ValueRef::Integer(i) => CellValue::Text(i.to_string()),
        ValueRef::Real(f) => CellValue::Text(f.to_string()),
        ValueRef::Text(t) => CellValue::Text(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => CellValue::Bytes(b.to_vec()),
    }
}

fn quote_string(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}


#[async_trait::async_trait]
impl sqlpro_core::write::ParameterizedExecutor for SqliteDriver {
    async fn execute_parameterized(
        &self,
        sql: &str,
        parameters: &[Option<sqlpro_core::value::BindValue>],
    ) -> sqlpro_core::error::Result<u64> {
        let conn = Arc::clone(&self.conn);
        let sql = sql.to_string();
        let params: Vec<Option<sqlpro_core::value::BindValue>> = parameters.to_vec();
        on_blocking(move || {
            let conn = conn
                .lock()
                .map_err(|_| sqlpro_core::error::Error::Driver("poisoned connection".into()))?;
            let mut prepared = conn
                .prepare(&sql)
                .map_err(|e| sqlpro_core::error::Error::Query(e.to_string()))?;
            let values: Vec<rusqlite::types::Value> = params
                .iter()
                .map(|p| match p {
                    None => rusqlite::types::Value::Null,
                    Some(sqlpro_core::value::BindValue::Text(s)) => {
                        rusqlite::types::Value::Text(s.clone())
                    }
                    Some(sqlpro_core::value::BindValue::Bytes(b)) => {
                        rusqlite::types::Value::Blob(b.clone())
                    }
                })
                .collect();
            let count = prepared
                .execute(rusqlite::params_from_iter(values.iter()))
                .map_err(|e| sqlpro_core::error::Error::Query(e.to_string()))?;
            Ok(count as u64)
        })
        .await
    }
}
