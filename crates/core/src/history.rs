//! Query history: a SQLite-backed record of executed queries with FTS5
//! search, keyset pagination, and retention.
//!
//! Port of TablePro's `QueryHistoryStorage` (schema, fingerprint grouping,
//! FTS query construction, retention "0 = unlimited" semantics) and the
//! `QueryHistoryEntry` model.

use std::path::{Path, PathBuf};

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::database_type::DatabaseType;
use crate::error::{Error, Result};
use crate::fingerprint::fingerprint_hash;

/// Where in the app a recorded query came from (TablePro's
/// `QueryHistorySource`, trimmed to the sources this port has).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryHistorySource {
    Editor,
    Explain,
    TableBrowse,
    RowEdit,
    StructureDdl,
    Import,
}

impl QueryHistorySource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Editor => "editor",
            Self::Explain => "explain",
            Self::TableBrowse => "table_browse",
            Self::RowEdit => "row_edit",
            Self::StructureDdl => "structure_ddl",
            Self::Import => "import",
        }
    }
}

/// The leading-keyword statement classification (TablePro's
/// `QueryHistoryStatementType.classify`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StatementType {
    Select,
    Insert,
    Update,
    Delete,
    Ddl,
    Other,
}

impl StatementType {
    pub fn classify(query: &str) -> Self {
        let leading = query
            .trim_start()
            .trim_start_matches(['(', ';'])
            .split_whitespace()
            .next()
            .unwrap_or("")
            .trim_start_matches(['(', ';'])
            .to_uppercase();

        match leading.as_str() {
            "SELECT" | "SHOW" | "DESCRIBE" | "DESC" | "EXPLAIN" | "WITH" | "TABLE" | "VALUES"
            | "PRAGMA" => Self::Select,
            "INSERT" | "REPLACE" | "UPSERT" | "MERGE" | "COPY" | "LOAD" => Self::Insert,
            "UPDATE" => Self::Update,
            "DELETE" | "TRUNCATE" => Self::Delete,
            "CREATE" | "ALTER" | "DROP" | "RENAME" | "COMMENT" | "GRANT" | "REVOKE" | "SET"
            | "ANALYZE" | "VACUUM" | "REINDEX" => Self::Ddl,
            _ => Self::Other,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Select => "select",
            Self::Insert => "insert",
            Self::Update => "update",
            Self::Delete => "delete",
            Self::Ddl => "ddl",
            Self::Other => "other",
        }
    }
}

/// One recorded query (TablePro's `QueryHistoryEntry`). `row_count` of -1
/// means unknown.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryHistoryEntry {
    pub id: String,
    pub query: String,
    pub connection_id: String,
    pub database_name: String,
    pub database_type: String,
    pub schema_name: Option<String>,
    pub source: String,
    pub statement_type: String,
    pub executed_at: f64,
    pub execution_time: f64,
    pub row_count: i64,
    pub was_successful: bool,
    pub error_message: Option<String>,
    pub fingerprint_hash: i64,
}

impl QueryHistoryEntry {
    pub fn new(query: impl Into<String>, connection_id: impl Into<String>, database_type: &DatabaseType) -> Self {
        let query = query.into();
        let statement_type = StatementType::classify(&query).as_str().to_string();
        Self {
            id: Uuid::new_v4().to_string(),
            fingerprint_hash: fingerprint_hash(&query),
            query,
            connection_id: connection_id.into(),
            database_name: String::new(),
            database_type: database_type.id().to_string(),
            schema_name: None,
            source: QueryHistorySource::Editor.as_str().to_string(),
            statement_type,
            executed_at: Utc::now().timestamp_millis() as f64 / 1000.0,
            execution_time: 0.0,
            row_count: -1,
            was_successful: true,
            error_message: None,
        }
    }
}

/// Search/pagination filter (TablePro's `QueryHistoryFilter`).
#[derive(Debug, Clone, Default)]
pub struct QueryHistoryFilter {
    pub connection_id: Option<String>,
    pub search_text: Option<String>,
    pub succeeded_only: bool,
    pub failed_only: bool,
    pub since: Option<f64>,
    pub until: Option<f64>,
}

impl QueryHistoryFilter {
    fn matches_nothing(&self) -> bool {
        self.since.is_some_and(|s| self.until.is_some_and(|u| s > u))
    }
}

/// Retention settings; 0 means unlimited (TablePro's semantics).
#[derive(Debug, Clone)]
pub struct RetentionSettings {
    pub max_entries: usize,
    pub max_days: u32,
}

impl Default for RetentionSettings {
    fn default() -> Self {
        Self { max_entries: 10_000, max_days: 90 }
    }
}

/// One page of history with a keyset cursor.
pub struct QueryHistoryPage {
    pub entries: Vec<QueryHistoryEntry>,
    /// Feed back into `fetch` to get the next page.
    pub next_cursor: Option<(f64, String)>,
}

/// SQLite-backed query history. Same schema, search, and retention
/// semantics as TablePro's storage.
pub struct QueryHistoryStorage {
    conn: Connection,
}

impl QueryHistoryStorage {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(Error::Io)?;
        }
        let conn = Connection::open(path).map_err(|e| Error::Driver(e.to_string()))?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().map_err(|e| Error::Driver(e.to_string()))?;
        Self::init(conn)
    }

    fn init(conn: Connection) -> Result<Self> {
        let pragma_batch = "
            PRAGMA journal_mode=WAL;
            PRAGMA synchronous=NORMAL;
        ";
        conn.execute_batch(pragma_batch).map_err(|e| Error::Driver(e.to_string()))?;

        let schema = "
            CREATE TABLE IF NOT EXISTS history (
                id TEXT PRIMARY KEY,
                query TEXT NOT NULL,
                connection_id TEXT NOT NULL,
                database_name TEXT NOT NULL,
                database_type TEXT NOT NULL DEFAULT '',
                schema_name TEXT,
                source TEXT NOT NULL DEFAULT 'editor',
                statement_type TEXT NOT NULL DEFAULT 'other',
                executed_at REAL NOT NULL,
                execution_time REAL NOT NULL,
                row_count INTEGER NOT NULL,
                was_successful INTEGER NOT NULL,
                error_message TEXT,
                fingerprint_hash INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_history_connection_executed
                ON history(connection_id, executed_at DESC);
            CREATE INDEX IF NOT EXISTS idx_history_executed_at ON history(executed_at DESC);
            CREATE INDEX IF NOT EXISTS idx_history_fingerprint_executed
                ON history(fingerprint_hash, executed_at DESC);
        ";
        conn.execute_batch(schema).map_err(|e| Error::Driver(e.to_string()))?;

        let fts = "
            CREATE VIRTUAL TABLE IF NOT EXISTS history_fts USING fts5(
                query, content='history', content_rowid='rowid'
            );
            CREATE TRIGGER IF NOT EXISTS history_ai AFTER INSERT ON history BEGIN
                INSERT INTO history_fts(rowid, query) VALUES (new.rowid, new.query);
            END;
            CREATE TRIGGER IF NOT EXISTS history_ad AFTER DELETE ON history BEGIN
                INSERT INTO history_fts(history_fts, rowid, query)
                    VALUES ('delete', old.rowid, old.query);
            END;
        ";
        // FTS5 may be unavailable in a minimal SQLite build; history still
        // works without search in that case.
        if let Err(e) = conn.execute_batch(fts) {
            eprintln!("warning: FTS5 unavailable, history search disabled: {e}");
        }

        Ok(Self { conn })
    }

    /// Record one executed query.
    pub fn record(&self, entry: &QueryHistoryEntry) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO history (id, query, connection_id, database_name, database_type,
                     schema_name, source, statement_type, executed_at, execution_time, row_count,
                     was_successful, error_message, fingerprint_hash)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                params![
                    entry.id,
                    entry.query,
                    entry.connection_id,
                    entry.database_name,
                    entry.database_type,
                    entry.schema_name,
                    entry.source,
                    entry.statement_type,
                    entry.executed_at,
                    entry.execution_time,
                    entry.row_count,
                    entry.was_successful as i64,
                    entry.error_message,
                    entry.fingerprint_hash,
                ],
            )
            .map_err(|e| Error::Driver(e.to_string()))?;
        Ok(())
    }

    /// Fetch one page, newest first, keyset-paginated by
    /// `(executed_at, id)` with the cursor predicate
    /// `(executed_at < ? OR (executed_at = ? AND id < ?))`.
    pub fn fetch(
        &self,
        filter: &QueryHistoryFilter,
        after: Option<(f64, String)>,
        limit: usize,
    ) -> Result<QueryHistoryPage> {
        if filter.matches_nothing() {
            return Ok(QueryHistoryPage { entries: Vec::new(), next_cursor: None });
        }

        let fts_available = self.fts_available();
        let mut sql = String::from("SELECT h.id, h.query, h.connection_id, h.database_name, h.database_type, h.schema_name, h.source, h.statement_type, h.executed_at, h.execution_time, h.row_count, h.was_successful, h.error_message, h.fingerprint_hash FROM history h");
        if filter.search_text.is_some() && fts_available {
            sql.push_str(" INNER JOIN history_fts ON h.rowid = history_fts.rowid AND history_fts MATCH ?1");
        }
        sql.push_str(" WHERE 1=1");
        let mut bind_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if let Some(text) = &filter.search_text {
            if fts_available {
                bind_values.push(Box::new(fts_query(text)));
            } else {
                sql.push_str(&format!(
                    " AND h.query LIKE ?{}",
                    bind_values.len() + 1
                ));
                bind_values.push(Box::new(format!("%{}%", text.replace('%', "\\%"))));
            }
        }
        if let Some(connection_id) = &filter.connection_id {
            sql.push_str(&format!(" AND h.connection_id = ?{}", bind_values.len() + 1));
            bind_values.push(Box::new(connection_id.clone()));
        }
        if filter.succeeded_only {
            sql.push_str(" AND h.was_successful = 1");
        }
        if filter.failed_only {
            sql.push_str(" AND h.was_successful = 0");
        }
        if let Some(since) = filter.since {
            sql.push_str(&format!(" AND h.executed_at >= ?{}", bind_values.len() + 1));
            bind_values.push(Box::new(since));
        }
        if let Some(until) = filter.until {
            sql.push_str(&format!(" AND h.executed_at <= ?{}", bind_values.len() + 1));
            bind_values.push(Box::new(until));
        }
        if let Some((executed_at, id)) = after {
            sql.push_str(&format!(
                " AND (h.executed_at < ?{} OR (h.executed_at = ?{} AND h.id < ?{}))",
                bind_values.len() + 1,
                bind_values.len() + 2,
                bind_values.len() + 3
            ));
            bind_values.push(Box::new(executed_at));
            bind_values.push(Box::new(executed_at));
            bind_values.push(Box::new(id));
        }
        sql.push_str(&format!(" ORDER BY h.executed_at DESC, h.id DESC LIMIT ?{}", bind_values.len() + 1));
        bind_values.push(Box::new(limit as i64 + 1));

        let mut stmt = self.conn.prepare(&sql).map_err(|e| Error::Driver(e.to_string()))?;
        let raw_params: Vec<&dyn rusqlite::types::ToSql> =
            bind_values.iter().map(|b| b.as_ref() as &dyn rusqlite::types::ToSql).collect();

        let rows = stmt
            .query_map(rusqlite::params_from_iter(raw_params.iter()), row_to_entry)
            .map_err(|e| Error::Driver(e.to_string()))?;

        let mut entries: Vec<QueryHistoryEntry> = Vec::new();
        for row in rows {
            entries.push(row.map_err(|e| Error::Driver(e.to_string()))?);
        }

        let mut next_cursor = None;
        if entries.len() > limit {
            entries.truncate(limit);
            let last = entries.last().unwrap();
            next_cursor = Some((last.executed_at, last.id.clone()));
        }

        Ok(QueryHistoryPage { entries, next_cursor })
    }

    pub fn count(&self, filter: &QueryHistoryFilter) -> Result<i64> {
        if filter.matches_nothing() {
            return Ok(0);
        }
        let count = if let Some(connection_id) = &filter.connection_id {
            self.conn
                .query_row(
                    "SELECT COUNT(*) FROM history WHERE connection_id = ?1",
                    params![connection_id],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| Error::Driver(e.to_string()))?
                .unwrap_or(0)
        } else {
            self.conn
                .query_row("SELECT COUNT(*) FROM history", [], |r| r.get(0))
                .optional()
                .map_err(|e| Error::Driver(e.to_string()))?
                .unwrap_or(0)
        };
        Ok(count)
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM history WHERE id = ?1", params![id])
            .map_err(|e| Error::Driver(e.to_string()))?;
        Ok(())
    }

    /// Clear everything for a connection (TablePro also clears snapshots;
    /// this port has none yet).
    pub fn delete_everything(&self, connection_id: Option<&str>) -> Result<()> {
        match connection_id {
            Some(id) => self
                .conn
                .execute("DELETE FROM history WHERE connection_id = ?1", params![id])
                .map_err(|e| Error::Driver(e.to_string()))?,
            None => self
                .conn
                .execute("DELETE FROM history", [])
                .map_err(|e| Error::Driver(e.to_string()))?,
        };
        Ok(())
    }

    /// Retention: delete by age, then keep only the newest `max_entries`
    /// under the read ordering. 0 means unlimited. Returns whether anything
    /// changed.
    pub fn perform_cleanup(&self, settings: &RetentionSettings) -> Result<bool> {
        let mut changed = false;
        if settings.max_days > 0 {
            let cutoff = Utc::now().timestamp() as f64 - (settings.max_days as f64) * 86_400.0;
            let n = self
                .conn
                .execute("DELETE FROM history WHERE executed_at < ?1", params![cutoff])
                .map_err(|e| Error::Driver(e.to_string()))?;
            changed |= n > 0;
        }
        if settings.max_entries > 0 {
            let n = self
                .conn
                .execute(
                    "DELETE FROM history WHERE id IN (
                        SELECT id FROM history
                        ORDER BY executed_at DESC, id DESC
                        LIMIT -1 OFFSET ?1
                    )",
                    params![settings.max_entries as i64],
                )
                .map_err(|e| Error::Driver(e.to_string()))?;
            changed |= n > 0;
        }
        Ok(changed)
    }

    fn fts_available(&self) -> bool {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name = 'history_fts'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n > 0)
            .unwrap_or(false)
    }
}

/// Default database file location: `~/.local/share/sqlpro/query_history.db`
/// (matches this port's connections store).
pub fn default_history_path() -> PathBuf {
    let mut base = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    base.push(".local/share/sqlpro");
    base.push("query_history.db");
    base
}

/// TablePro's `ftsQuery`: every whitespace word becomes a quoted prefix
/// term, ANDed. Internal `"` is doubled. Quoting each term disables the FTS5
/// operators, so searching "AND" is text.
fn fts_query(search_text: &str) -> String {
    let terms: Vec<String> = search_text
        .split_whitespace()
        .map(|term| format!("\"{}\"*", term.replace('"', "\"\"")))
        .collect();
    if terms.is_empty() {
        format!("\"{}\"*", search_text.replace('"', "\"\""))
    } else {
        terms.join(" ")
    }
}

fn row_to_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<QueryHistoryEntry> {
    Ok(QueryHistoryEntry {
        id: row.get(0)?,
        query: row.get(1)?,
        connection_id: row.get(2)?,
        database_name: row.get(3)?,
        database_type: row.get(4)?,
        schema_name: row.get(5)?,
        source: row.get(6)?,
        statement_type: row.get(7)?,
        executed_at: row.get(8)?,
        execution_time: row.get(9)?,
        row_count: row.get(10)?,
        was_successful: row.get::<_, i64>(11)? != 0,
        error_message: row.get(12)?,
        fingerprint_hash: row.get(13)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn storage() -> QueryHistoryStorage {
        QueryHistoryStorage::open_in_memory().unwrap()
    }

    fn entry(query: &str, connection: &str, success: bool) -> QueryHistoryEntry {
        let mut e = QueryHistoryEntry::new(
            query,
            connection,
            &DatabaseType::new(DatabaseType::SQLITE),
        );
        e.was_successful = success;
        e
    }

    #[test]
    fn records_and_reads_back() {
        let s = storage();
        s.record(&entry("SELECT 1", "conn-1", true)).unwrap();
        let page = s.fetch(&QueryHistoryFilter::default(), None, 10).unwrap();
        assert_eq!(page.entries.len(), 1);
        assert_eq!(page.entries[0].query, "SELECT 1");
        assert_eq!(page.entries[0].statement_type, "select");
    }

    #[test]
    fn newest_first() {
        let s = storage();
        let mut a = entry("SELECT 1", "c", true);
        a.executed_at = 100.0;
        let mut b = entry("SELECT 2", "c", true);
        b.executed_at = 200.0;
        s.record(&a).unwrap();
        s.record(&b).unwrap();
        let page = s.fetch(&QueryHistoryFilter::default(), None, 10).unwrap();
        assert_eq!(page.entries[0].query, "SELECT 2");
        assert_eq!(page.entries[1].query, "SELECT 1");
    }

    #[test]
    fn keyset_pagination() {
        let s = storage();
        for i in 0..5 {
            let mut e = entry(&format!("SELECT {i}"), "c", true);
            e.executed_at = 100.0 + i as f64;
            s.record(&e).unwrap();
        }
        let first = s.fetch(&QueryHistoryFilter::default(), None, 2).unwrap();
        assert_eq!(first.entries.len(), 2);
        assert!(first.next_cursor.is_some());
        let second = s
            .fetch(&QueryHistoryFilter::default(), first.next_cursor, 2)
            .unwrap();
        assert_eq!(second.entries.len(), 2);
        // No overlap between pages.
        assert_ne!(first.entries[0].id, second.entries[0].id);
    }

    #[test]
    fn search_via_fts() {
        let s = storage();
        s.record(&entry("SELECT * FROM customers", "c", true)).unwrap();
        s.record(&entry("SELECT * FROM orders", "c", true)).unwrap();
        let filter = QueryHistoryFilter { search_text: Some("customers".into()), ..Default::default() };
        let page = s.fetch(&filter, None, 10).unwrap();
        assert_eq!(page.entries.len(), 1);
        assert!(page.entries[0].query.contains("customers"));
    }

    #[test]
    fn search_is_prefix_per_word() {
        let s = storage();
        s.record(&entry("SELECT * FROM customers", "c", true)).unwrap();
        let filter = QueryHistoryFilter { search_text: Some("cust".into()), ..Default::default() };
        assert_eq!(s.fetch(&filter, None, 10).unwrap().entries.len(), 1);
    }

    #[test]
    fn scope_filters_by_connection() {
        let s = storage();
        s.record(&entry("SELECT 1", "conn-1", true)).unwrap();
        s.record(&entry("SELECT 2", "conn-2", true)).unwrap();
        let filter = QueryHistoryFilter { connection_id: Some("conn-1".into()), ..Default::default() };
        let page = s.fetch(&filter, None, 10).unwrap();
        assert_eq!(page.entries.len(), 1);
        assert_eq!(page.entries[0].connection_id, "conn-1");
    }

    #[test]
    fn outcome_filter() {
        let s = storage();
        s.record(&entry("SELECT 1", "c", true)).unwrap();
        s.record(&entry("SELECT bad", "c", false)).unwrap();
        let ok = QueryHistoryFilter { succeeded_only: true, ..Default::default() };
        assert_eq!(s.fetch(&ok, None, 10).unwrap().entries.len(), 1);
        let bad = QueryHistoryFilter { failed_only: true, ..Default::default() };
        assert_eq!(s.fetch(&bad, None, 10).unwrap().entries.len(), 1);
    }

    #[test]
    fn count_scopes() {
        let s = storage();
        s.record(&entry("SELECT 1", "conn-1", true)).unwrap();
        s.record(&entry("SELECT 2", "conn-1", true)).unwrap();
        s.record(&entry("SELECT 3", "conn-2", true)).unwrap();
        assert_eq!(s.count(&QueryHistoryFilter::default()).unwrap(), 3);
        let scoped = QueryHistoryFilter { connection_id: Some("conn-1".into()), ..Default::default() };
        assert_eq!(s.count(&scoped).unwrap(), 2);
    }

    #[test]
    fn retention_keeps_newest_max_entries() {
        let s = storage();
        for i in 0..10 {
            let mut e = entry(&format!("SELECT {i}"), "c", true);
            e.executed_at = 100.0 + i as f64;
            s.record(&e).unwrap();
        }
        s.perform_cleanup(&RetentionSettings { max_entries: 3, max_days: 0 }).unwrap();
        assert_eq!(s.count(&QueryHistoryFilter::default()).unwrap(), 3);
        let page = s.fetch(&QueryHistoryFilter::default(), None, 10).unwrap();
        // The newest three survive.
        assert_eq!(page.entries[0].query, "SELECT 9");
        assert_eq!(page.entries[2].query, "SELECT 7");
    }

    #[test]
    fn retention_by_age() {
        let s = storage();
        let mut old = entry("SELECT old", "c", true);
        old.executed_at = 1.0;
        let mut recent = entry("SELECT new", "c", true);
        recent.executed_at = Utc::now().timestamp() as f64;
        s.record(&old).unwrap();
        s.record(&recent).unwrap();
        s.perform_cleanup(&RetentionSettings { max_entries: 0, max_days: 7 }).unwrap();
        let page = s.fetch(&QueryHistoryFilter::default(), None, 10).unwrap();
        assert_eq!(page.entries.len(), 1);
        assert_eq!(page.entries[0].query, "SELECT new");
    }

    #[test]
    fn delete_one_row() {
        let s = storage();
        let e = entry("SELECT 1", "c", true);
        s.record(&e).unwrap();
        s.delete(&e.id).unwrap();
        assert_eq!(s.count(&QueryHistoryFilter::default()).unwrap(), 0);
    }

    #[test]
    fn statement_type_classification() {
        assert_eq!(StatementType::classify("SELECT 1"), StatementType::Select);
        assert_eq!(StatementType::classify("  insert into t …"), StatementType::Insert);
        assert_eq!(StatementType::classify("UPDATE t SET a = 1"), StatementType::Update);
        assert_eq!(StatementType::classify("DELETE FROM t"), StatementType::Delete);
        assert_eq!(StatementType::classify("CREATE TABLE t (a)"), StatementType::Ddl);
        assert_eq!(StatementType::classify("EXPLAIN SELECT 1"), StatementType::Select);
        assert_eq!(StatementType::classify("WITH c AS (SELECT 1) SELECT * FROM c"), StatementType::Select);
        assert_eq!(StatementType::classify("PRAGMA table_info(t)"), StatementType::Select);
        assert_eq!(StatementType::classify(""), StatementType::Other);
    }

    #[test]
    fn fingerprint_is_stable_and_recorded() {
        let a = entry("SELECT * FROM users WHERE id = 1", "c", true);
        let b = entry("SELECT * FROM users WHERE id = 999", "c", true);
        assert_eq!(a.fingerprint_hash, b.fingerprint_hash);
        assert_ne!(a.fingerprint_hash, 0);
    }
}
