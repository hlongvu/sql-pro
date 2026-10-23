//! MySQL / MariaDB driver over mysql_async. TiDB, OceanBase and Databend
//! speak the same protocol, as do MariaDB's forks.

use std::time::Instant;

use sqlpro_core::value::CellValue;
use sqlpro_core::connection::ConnectionConfig;
use sqlpro_core::driver::DatabaseDriver;
use sqlpro_core::error::{Error, Result};
use sqlpro_core::result::QueryResult;
use sqlpro_core::schema::{ColumnInfo, TableInfo};

use mysql_async::consts::ColumnFlags;
use mysql_async::prelude::*;
use mysql_async::{PoolConstraints, PoolOpts};

use crate::runtime::on_tokio;
use crate::sql_split::split_statements;

pub struct MySqlDriver {
    pool: mysql_async::Pool,
}

impl MySqlDriver {
    pub fn connect(config: &ConnectionConfig) -> Result<Self> {
        let host = config
            .host
            .clone()
            .ok_or_else(|| Error::Connection("MySQL connection has no host".into()))?;
        let port = config.port().ok_or_else(|| Error::Connection("MySQL connection has no port".into()))?;
        let username = config.username.clone().unwrap_or_else(|| "root".into());
        let password = config.password.clone().unwrap_or_default();
        let database = config.database.clone().unwrap_or_default();
        let socket = config.path.clone();

        let pool_opts = PoolOpts::default().with_constraints(
            PoolConstraints::new(0, 1).expect("a one-connection pool is valid"),
        );
        let mut builder = mysql_async::OptsBuilder::default()
            .ip_or_hostname(host)
            .tcp_port(port)
            .user(Some(username))
            .pass(Some(password))
            .pool_opts(pool_opts);

        if !database.is_empty() {
            builder = builder.db_name(Some(database));
        }
        if let Some(socket) = socket {
            // Reuse the path field for a unix socket, like TablePro lets
            // MySQL connections specify a socket file.
            if socket.starts_with('/') {
                builder = builder.socket(Some(socket));
            }
        }

        // A 1-connection pool keeps session state (USE, temp tables)
        // coherent while letting the driver trait take &self.
        let pool = mysql_async::Pool::new(builder);
        Ok(Self { pool })
    }
}

#[async_trait::async_trait]
impl DatabaseDriver for MySqlDriver {
    async fn ping(&self) -> Result<()> {
        self.execute("SELECT 1").await.map(|_| ())
    }

    async fn execute(&self, sql: &str) -> Result<QueryResult> {
        let statements = split_statements(sql);
        if statements.is_empty() {
            return Ok(QueryResult::empty(std::time::Duration::ZERO));
        }

        let started = Instant::now();
        let mut last_rows: Option<(Vec<ColumnInfo>, Vec<Vec<CellValue>>)> = None;
        let mut total_affected: u64 = 0;

        for stmt in &statements {
            let (rows, affected) = {
                let pool = self.pool.clone();
                let stmt = stmt.clone();
                on_tokio(async move {
                    let mut conn = pool
                        .get_conn()
                        .await
                        .map_err(|e| Error::Query(e.to_string()))?;
                    run_statement(&mut conn, &stmt).await
                })
                .await?
            };
            total_affected += affected;
            if let Some((columns, rows)) = rows {
                last_rows = Some((columns, rows));
            }
        }

        Ok(match last_rows {
            Some((columns, rows)) => QueryResult {
                columns,
                rows,
                rows_affected: total_affected,
                duration: started.elapsed(),
                is_query: true,
            },
            None => QueryResult {
                columns: Vec::new(),
                rows: Vec::new(),
                rows_affected: total_affected,
                duration: started.elapsed(),
                is_query: false,
            },
        })
    }

    async fn list_tables(&self) -> Result<Vec<TableInfo>> {
        let result = self
            .execute(
                "SELECT table_name, table_type, table_rows \
                 FROM information_schema.tables \
                 WHERE table_schema = DATABASE() ORDER BY table_type, table_name",
            )
            .await?;
        let mut tables = Vec::new();
        for row in &result.rows {
            let name = match row.first() {
                Some(CellValue::Text(s)) => s.clone(),
                _ => continue,
            };
            let is_view = matches!(row.get(1), Some(CellValue::Text(t)) if t != "BASE TABLE");
            let row_estimate = match row.get(2) {
                Some(CellValue::Text(s)) => s.parse::<u64>().ok(),
                _ => None,
            };
            tables.push(TableInfo { name, is_view, row_estimate });
        }
        Ok(tables)
    }

    async fn list_columns(&self, table: &str) -> Result<Vec<ColumnInfo>> {
        let result = self
            .execute(&format!(
                "SELECT column_name, column_type, is_nullable, column_key \
                 FROM information_schema.columns \
                 WHERE table_schema = DATABASE() AND table_name = {} \
                 ORDER BY ordinal_position",
                quote_string(table)
            ))
            .await?;
        let mut columns = Vec::new();
        for row in &result.rows {
            let (Some(CellValue::Text(name)), dtype, nullable, key) =
                (row.first(), row.get(1), row.get(2), row.get(3))
            else {
                continue;
            };
            columns.push(ColumnInfo {
                name: name.clone(),
                data_type: match dtype {
                    Some(CellValue::Text(s)) => s.clone(),
                    _ => String::new(),
                },
                nullable: matches!(nullable, Some(CellValue::Text(s)) if s == "YES"),
                is_primary_key: matches!(key, Some(CellValue::Text(k)) if k == "PRI"),
            });
        }
        Ok(columns)
    }

    fn label(&self) -> String {
        "MySQL".to_string()
    }
}

async fn run_statement<Q>(
    conn: &mut Q,
    stmt: &str,
) -> Result<(Option<(Vec<ColumnInfo>, Vec<Vec<CellValue>>)>, u64)>
where
    Q: Queryable + Send,
{
    let result = conn
        .query_iter(stmt)
        .await
        .map_err(|e| Error::Query(e.to_string()))?;

    let affected = result.affected_rows();
    let Some(columns_arc) = result.columns() else {
        // Write statement: drain so the connection is usable for the next.
        result
            .collect_and_drop::<mysql_async::Row>()
            .await
            .map_err(|e| Error::Query(e.to_string()))?;
        return Ok((None, affected));
    };

    let columns: Vec<ColumnInfo> = columns_arc
        .iter()
        .map(|c| ColumnInfo {
            name: c.name_str().into_owned(),
            data_type: format!("{:?}", c.column_type()),
            nullable: !c.flags().contains(ColumnFlags::NOT_NULL_FLAG),
            is_primary_key: c.flags().contains(ColumnFlags::PRI_KEY_FLAG),
        })
        .collect();

    let rows = result
        .collect_and_drop::<mysql_async::Row>()
        .await
        .map_err(|e| Error::Query(e.to_string()))?;

    let mut out = Vec::new();
    for row in rows {
        let mut cells = Vec::with_capacity(row.len());
        for v in row.unwrap() {
            cells.push(value_to_cell(v));
        }
        out.push(cells);
    }
    Ok((Some((columns, out)), affected))
}

fn value_to_cell(v: mysql_async::Value) -> CellValue {
    use mysql_async::Value;
    match v {
        Value::NULL => CellValue::Null,
        Value::Bytes(b) => CellValue::Text(String::from_utf8_lossy(&b).into_owned()),
        Value::Int(i) => CellValue::Text(i.to_string()),
        Value::UInt(u) => CellValue::Text(u.to_string()),
        Value::Float(f) => CellValue::Text((f as f64).to_string()),
        Value::Double(f) => CellValue::Text(f.to_string()),
        // Dates, times, decimals render to their protocol text form.
        other => CellValue::Text(other.as_sql(true)),
    }
}

fn quote_string(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

struct MySqlWriteSession {
    conn: Option<mysql_async::Conn>,
}

impl MySqlWriteSession {
    fn conn_mut(&mut self) -> Result<&mut mysql_async::Conn> {
        self.conn
            .as_mut()
            .ok_or_else(|| Error::Driver("MySQL write session is closed".into()))
    }
}

#[async_trait::async_trait]
impl sqlpro_core::write::ParameterizedSession for MySqlWriteSession {
    async fn execute(&mut self, sql: &str) -> Result<()> {
        self.conn_mut()?
            .query_drop(sql)
            .await
            .map_err(|e| Error::Query(e.to_string()))
    }

    async fn execute_parameterized(
        &mut self,
        sql: &str,
        parameters: &[Option<sqlpro_core::value::BindValue>],
    ) -> Result<u64> {
        execute_parameterized_on(self.conn_mut()?, sql, parameters).await
    }
}

#[async_trait::async_trait]
impl sqlpro_core::write::ParameterizedExecutor for MySqlDriver {
    async fn execute_parameterized(
        &self,
        sql: &str,
        parameters: &[Option<sqlpro_core::value::BindValue>],
    ) -> sqlpro_core::error::Result<u64> {
        let pool = self.pool.clone();
        let sql = sql.to_string();
        let params = parameters.to_vec();
        on_tokio(async move {
            let mut conn = pool
                .get_conn()
                .await
                .map_err(|e| Error::Query(e.to_string()))?;
            execute_parameterized_on(&mut conn, &sql, &params).await
        })
        .await
    }

    async fn write_session(
        &self,
    ) -> Result<Box<dyn sqlpro_core::write::ParameterizedSession + '_>> {
        let pool = self.pool.clone();
        let conn = on_tokio(async move {
            pool.get_conn()
                .await
                .map_err(|e| Error::Query(e.to_string()))
        })
        .await?;
        Ok(Box::new(MySqlWriteSession { conn: Some(conn) }))
    }
}

async fn execute_parameterized_on<Q>(
    conn: &mut Q,
    sql: &str,
    parameters: &[Option<sqlpro_core::value::BindValue>],
) -> Result<u64>
where
    Q: Queryable + Send,
{
    let values: Vec<mysql_async::Value> = parameters
        .iter()
        .map(|parameter| match parameter {
            None => mysql_async::Value::NULL,
            Some(sqlpro_core::value::BindValue::Text(value)) => {
                mysql_async::Value::Bytes(value.as_bytes().to_vec())
            }
            Some(sqlpro_core::value::BindValue::Bytes(value)) => {
                mysql_async::Value::Bytes(value.clone())
            }
        })
        .collect();
    let result = conn
        .exec_iter(sql, values)
        .await
        .map_err(|e| Error::Query(e.to_string()))?;
    let affected = result.affected_rows();
    result
        .collect_and_drop::<mysql_async::Row>()
        .await
        .map_err(|e| Error::Query(e.to_string()))?;
    Ok(affected)
}
