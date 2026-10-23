//! PostgreSQL driver over tokio-postgres (NoTls for now; TLS modes are a
//! later milestone). Redshift and CockroachDB ride the same wire protocol,
//! like TablePro routes them through libpq.

use std::time::{Duration, Instant};

use sqlpro_core::value::CellValue;
use sqlpro_core::connection::ConnectionConfig;
use sqlpro_core::driver::DatabaseDriver;
use sqlpro_core::error::{Error, Result};
use sqlpro_core::result::QueryResult;
use sqlpro_core::schema::{ColumnInfo, TableInfo};

use crate::runtime::on_tokio;
use crate::sql_split::split_statements;

pub struct PostgresDriver {
    // tokio-postgres Client is pipelined and shareable, so the grid can run
    // schema lookups while a query is still streaming.
    client: std::sync::Arc<tokio_postgres::Client>,
}

impl PostgresDriver {
    pub fn connect(config: &ConnectionConfig) -> Result<Self> {
        let host = config
            .host
            .clone()
            .ok_or_else(|| Error::Connection("PostgreSQL connection has no host".into()))?;
        let port = config.port().ok_or_else(|| Error::Connection("PostgreSQL connection has no port".into()))?;
        let username = config
            .username
            .clone()
            .unwrap_or_else(|| whoami_username());
        let database = config.database.clone().unwrap_or_else(|| username.clone());
        let password = config.password.clone().unwrap_or_default();

        let mut pg_config = tokio_postgres::Config::new();
        pg_config
            .host(&host)
            .port(port)
            .user(&username)
            .password(password.as_bytes())
            .dbname(&database)
            .connect_timeout(Duration::from_secs(config.query_timeout_secs));

        // spawn + lightweight wait instead of block_on: create_driver may be
        // called from inside the shared Tokio runtime (via on_tokio), where
        // block_on would panic.
        let client = futures::executor::block_on(async move {
            let handle = crate::runtime::runtime().spawn(async move {
                let (client, connection) = pg_config
                    .connect(tokio_postgres::NoTls)
                    .await
                    .map_err(|e| Error::Connection(e.to_string()))?;
                tokio::spawn(async move {
                    if let Err(e) = connection.await {
                        eprintln!("postgres connection closed: {e}");
                    }
                });
                Ok::<_, Error>(client)
            });
            handle.await.unwrap_or_else(|e| Err(Error::Driver(e.to_string())))
        })?;

        Ok(Self { client: std::sync::Arc::new(client) })
    }
}

fn whoami_username() -> String {
    std::env::var("USER").unwrap_or_else(|_| "postgres".to_string())
}

#[async_trait::async_trait]
impl DatabaseDriver for PostgresDriver {
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
                let client = std::sync::Arc::clone(&self.client);
                let stmt = stmt.clone();
                on_tokio(async move {
                    run_statement(client.as_ref(), &stmt).await
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
                "SELECT c.relname, c.relkind, GREATEST(c.reltuples, 0)::bigint \
                 FROM pg_class c \
                 JOIN pg_namespace n ON n.oid = c.relnamespace \
                 WHERE n.nspname = current_schema() \
                   AND c.relkind IN ('r', 'p', 'v') \
                 ORDER BY c.relkind, c.relname",
            )
            .await?;
        let mut tables = Vec::new();
        for row in &result.rows {
            let (Some(CellValue::Text(name)), kind, estimate) = (row.first(), row.get(1), row.get(2))
            else {
                continue;
            };
            let is_view = matches!(kind, Some(CellValue::Text(k)) if k == "v");
            let row_estimate = match estimate {
                Some(CellValue::Text(s)) => s.parse::<u64>().ok(),
                _ => None,
            };
            tables.push(TableInfo { name: name.clone(), is_view, row_estimate });
        }
        Ok(tables)
    }

    async fn list_columns(&self, table: &str) -> Result<Vec<ColumnInfo>> {
        let result = self
            .execute(&format!(
                "SELECT c.column_name, c.data_type, c.is_nullable, \
                        COALESCE((SELECT true FROM information_schema.table_constraints tc \
                                  JOIN information_schema.key_column_usage kcu \
                                    ON tc.constraint_name = kcu.constraint_name \
                                   AND tc.table_schema = kcu.table_schema \
                                  WHERE tc.constraint_type = 'PRIMARY KEY' \
                                    AND tc.table_name = c.table_name \
                                    AND tc.table_schema = c.table_schema \
                                    AND kcu.column_name = c.column_name), false) AS is_pk \
                 FROM information_schema.columns c \
                 WHERE table_schema = current_schema() AND table_name = {} \
                 ORDER BY ordinal_position",
                quote_string(table)
            ))
            .await?;
        let mut columns = Vec::new();
        for row in &result.rows {
            let (Some(CellValue::Text(name)), dtype, nullable, pk) =
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
                is_primary_key: matches!(pk, Some(CellValue::Text(b)) if b == "true"),
            });
        }
        Ok(columns)
    }

    fn label(&self) -> String {
        "PostgreSQL".to_string()
    }
}

/// Prepare to learn the result shape, then query (row-returning) or execute
/// (write statement), so affected-row counts are correct.
async fn run_statement(
    client: &tokio_postgres::Client,
    stmt: &str,
) -> Result<(Option<(Vec<ColumnInfo>, Vec<Vec<CellValue>>)>, u64)> {
    let prepared = client
        .prepare(stmt)
        .await
        .map_err(|e| Error::Query(e.to_string()))?;

    if prepared.columns().is_empty() {
        let affected = client
            .execute(&prepared, &[])
            .await
            .map_err(|e| Error::Query(e.to_string()))?;
        return Ok((None, affected));
    }

    let columns: Vec<ColumnInfo> = prepared
        .columns()
        .iter()
        .map(|c| ColumnInfo {
            name: c.name().to_string(),
            data_type: c.type_().to_string(),
            nullable: false,
            is_primary_key: false,
        })
        .collect();

    let types: Vec<tokio_postgres::types::Type> = prepared
        .columns()
        .iter()
        .map(|c| c.type_().clone())
        .collect();

    let rows = client
        .query(&prepared, &[])
        .await
        .map_err(|e| Error::Query(e.to_string()))?;

    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let mut cells = Vec::with_capacity(types.len());
        for (i, ty) in types.iter().enumerate() {
            cells.push(pg_value_to_cell(row, i, ty));
        }
        out.push(cells);
    }
    Ok((Some((columns, out)), 0))
}

fn pg_value_to_cell(
    row: &tokio_postgres::Row,
    idx: usize,
    ty: &tokio_postgres::types::Type,
) -> CellValue {
    use tokio_postgres::types::Type;

    // NULL first: every typed attempt below returns Err for a NULL cell, so
    // detect it up front via a cheap Bool probe path.
    {
        let is_null = match ty {
            &Type::BOOL => matches!(row.try_get::<_, Option<bool>>(idx), Ok(None)),
            _ => matches!(
                row.try_get::<_, Option<String>>(idx),
                Ok(None) | Err(_)
            ) && {
                // try_get(String) errors for non-text types even when
                // non-null, so only trust Ok(None).
                matches!(row.try_get::<_, Option<String>>(idx), Ok(None))
            },
        };
        if is_null {
            return CellValue::Null;
        }
    }

    macro_rules! try_cell {
        ($t:ty, $map:expr) => {
            if let Ok(Some(v)) = row.try_get::<_, Option<$t>>(idx) {
                return $map(v);
            }
        };
    }

    match ty {
        &Type::BOOL => try_cell!(bool, |v: bool| CellValue::Text(v.to_string())),
        &Type::INT2 => try_cell!(i16, |v: i16| CellValue::Text(v.to_string())),
        &Type::INT4 => try_cell!(i32, |v: i32| CellValue::Text(v.to_string())),
        &Type::INT8 => try_cell!(i64, |v: i64| CellValue::Text(v.to_string())),
        &Type::FLOAT4 => try_cell!(f32, |v: f32| CellValue::Text((v as f64).to_string())),
        &Type::FLOAT8 => try_cell!(f64, |v: f64| CellValue::Text(v.to_string())),
        &Type::NUMERIC => try_cell!(
            rust_decimal::Decimal,
            |v: rust_decimal::Decimal| CellValue::Text(v.to_string())
        ),
        &Type::TEXT | &Type::VARCHAR | &Type::NAME | &Type::BPCHAR => {
            try_cell!(String, |v: String| CellValue::Text(v))
        }
        &Type::JSON | &Type::JSONB => {
            try_cell!(serde_json::Value, |v: serde_json::Value| CellValue::Text(v.to_string()))
        }
        &Type::BYTEA => try_cell!(Vec<u8>, |v: Vec<u8>| CellValue::Bytes(v)),
        &Type::UUID => try_cell!(uuid::Uuid, |v: uuid::Uuid| CellValue::Text(v.to_string())),
        &Type::TIMESTAMP => {
            try_cell!(chrono::NaiveDateTime, |v: chrono::NaiveDateTime| CellValue::Text(
                v.to_string()
            ))
        }
        &Type::TIME => {
            try_cell!(chrono::NaiveTime, |v: chrono::NaiveTime| CellValue::Text(v.to_string()))
        }
        &Type::DATE => try_cell!(chrono::NaiveDate, |v: chrono::NaiveDate| CellValue::Text(
            v.to_string()
        )),
        &Type::TIMESTAMPTZ => try_cell!(
            chrono::DateTime<chrono::Utc>,
            |v: chrono::DateTime<chrono::Utc>| CellValue::Text(v.to_string())
        ),
        _ => {}
    }

    // Enum types (USER-DEFINED) carry their variant name as plain text on
    // the wire, but tokio-postgres's String only accepts the text-ish OIDs,
    // so decode them through a permissive wrapper. Text-element arrays get
    // the same treatment in Postgres's {a,b} rendering.
    match ty.kind() {
        tokio_postgres::types::Kind::Enum(_) => {
            try_cell!(PgEnumText, |v: PgEnumText| CellValue::Text(v.0))
        }
        tokio_postgres::types::Kind::Array(elem)
            if <String as tokio_postgres::types::FromSql>::accepts(elem) =>
        {
            try_cell!(Vec<String>, |v: Vec<String>| CellValue::Text(format!(
                "{{{}}}",
                v.join(",")
            )))
        }
        _ => {}
    }

    // Fallback: ask Postgres for its text rendering of any other type.
    match row.try_get::<_, Option<String>>(idx) {
        Ok(Some(s)) => CellValue::Text(s),
        _ => CellValue::Null,
    }
}

fn quote_string(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// A permissive text decoder for enum types: an enum's binary
/// representation is its variant name as text, but `String` only accepts
/// the text-ish type OIDs, so enums need their own `FromSql`.
struct PgEnumText(String);

impl<'a> tokio_postgres::types::FromSql<'a> for PgEnumText {
    fn from_sql(
        _: &tokio_postgres::types::Type,
        raw: &'a [u8],
    ) -> std::result::Result<Self, Box<dyn std::error::Error + Sync + Send>> {
        Ok(PgEnumText(String::from_utf8_lossy(raw).into_owned()))
    }

    fn accepts(_: &tokio_postgres::types::Type) -> bool {
        true
    }
}


// The ParameterizedExecutor impl lives at the bottom, after PgParam: it
// binds through the type-adapting wrapper instead of raw strings.

/// A bind parameter that adapts to the type Postgres reports for its
/// placeholder. The grid reads every value as text (TablePro's model), but
/// tokio-postgres uses the binary wire format: when a statement context
/// pins the parameter type (a `uuid` PK, an `int4` id, a `bool` flag), a
/// plain `String` fails to serialize. This wrapper re-parses the text into
/// the reported type, falling back to text for text-ish types.
#[derive(Debug)]
struct PgParam {
    raw: Option<sqlpro_core::value::BindValue>,
}

impl<'a> tokio_postgres::types::ToSql for PgParam {
    fn to_sql(
        &self,
        ty: &tokio_postgres::types::Type,
        out: &mut tokio_postgres::types::private::BytesMut,
    ) -> std::result::Result<
        tokio_postgres::types::IsNull,
        Box<dyn std::error::Error + Sync + Send>,
    > {
                use tokio_postgres::types::{IsNull, Type, ToSql as _};
        use std::str::FromStr;

        let Some(raw) = &self.raw else {
            return Ok(IsNull::Yes);
        };
        let text = match raw {
            sqlpro_core::value::BindValue::Text(s) => s.clone(),
            sqlpro_core::value::BindValue::Bytes(b) => {
                return b.to_sql(&Type::BYTEA, out);
            }
        };

        match *ty {
            Type::INT2 => {
                if let Ok(v) = i16::from_str(&text) {
                    return v.to_sql(ty, out);
                }
            }
            Type::INT4 => {
                if let Ok(v) = i32::from_str(&text) {
                    return v.to_sql(ty, out);
                }
            }
            Type::INT8 => {
                if let Ok(v) = i64::from_str(&text) {
                    return v.to_sql(ty, out);
                }
            }
            Type::FLOAT4 => {
                if let Ok(v) = f32::from_str(&text) {
                    return v.to_sql(ty, out);
                }
            }
            Type::FLOAT8 => {
                if let Ok(v) = f64::from_str(&text) {
                    return v.to_sql(ty, out);
                }
            }
            Type::NUMERIC => {
                if let Ok(v) = rust_decimal::Decimal::from_str(&text) {
                    return v.to_sql(ty, out);
                }
            }
            Type::BOOL => {
                if let Ok(v) = bool::from_str(&text) {
                    return v.to_sql(ty, out);
                }
            }
            Type::JSON | Type::JSONB => {
                if let Ok(v) = serde_json::Value::from_str(&text) {
                    return v.to_sql(ty, out);
                }
            }
            Type::UUID => {
                if let Ok(v) = uuid::Uuid::from_str(&text) {
                    return v.to_sql(ty, out);
                }
            }
            Type::DATE => {
                if let Ok(v) = chrono::NaiveDate::parse_from_str(&text, "%Y-%m-%d") {
                    return v.to_sql(ty, out);
                }
            }
            Type::TIMESTAMP => {
                for fmt in ["%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%d %H:%M:%S", "%Y-%m-%dT%H:%M:%S%.f"] {
                    if let Ok(v) = chrono::NaiveDateTime::parse_from_str(&text, fmt) {
                        return v.to_sql(ty, out);
                    }
                }
            }
            Type::TIMESTAMPTZ => {
                // The grid displays timestamptz as "2026-10-03 05:09:12.947
                // UTC"; also accept offset and RFC3339 shapes. A value with
                // no zone is taken as UTC.
                let normalized = text.replace(" UTC", "+00:00");
                for fmt in [
                    "%Y-%m-%d %H:%M:%S%.f%:z",
                    "%Y-%m-%d %H:%M:%S%:z",
                    "%Y-%m-%dT%H:%M:%S%.f%:z",
                    "%+",
                ] {
                    if let Ok(v) = chrono::DateTime::<chrono::FixedOffset>::parse_from_str(
                        &normalized, fmt,
                    ) {
                        return v.to_sql(ty, out);
                    }
                }
                for fmt in ["%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%d %H:%M:%S"] {
                    if let Ok(v) = chrono::NaiveDateTime::parse_from_str(&text, fmt) {
                        return v.and_utc().to_sql(ty, out);
                    }
                }
            }
            _ => {}
        }

        // Text-ish types (and unparseable values): send as text. Postgres
        // coerces when the column accepts text; unknown types accept text.
        text.to_sql(ty, out)
    }

    fn accepts(_ty: &tokio_postgres::types::Type) -> bool {
        true
    }

    tokio_postgres::types::to_sql_checked!();
}

#[async_trait::async_trait]
impl sqlpro_core::write::ParameterizedExecutor for PostgresDriver {
    async fn write_session(
        &self,
    ) -> sqlpro_core::error::Result<Box<dyn sqlpro_core::write::ParameterizedSession + '_>> {
        Ok(sqlpro_core::write::default_write_session(self))
    }

    async fn execute_parameterized(
        &self,
        sql: &str,
        parameters: &[Option<sqlpro_core::value::BindValue>],
    ) -> sqlpro_core::error::Result<u64> {
        use tokio_postgres::types::ToSql;

        let client = std::sync::Arc::clone(&self.client);
        let sql = sql.to_string();
        let params: Vec<Option<sqlpro_core::value::BindValue>> = parameters.to_vec();
        on_tokio(async move {
            let binds: Vec<PgParam> = params
                .into_iter()
                .map(|p| PgParam { raw: p })
                .collect();
            let refs: Vec<&(dyn ToSql + Sync)> = binds
                .iter()
                .map(|b| b as &(dyn ToSql + Sync))
                .collect();
            let n = client
                .execute(&sql, refs.as_slice())
                .await
                .map_err(|e| sqlpro_core::error::Error::Query(e.to_string()))?;
            Ok(n)
        })
        .await
    }
}
