//! The main window, laid out per `ui_detail.md`:
//!
//! - Application toolbar (46–52 px): pane toggles, connection pill, + SQL,
//!   Run, Save, Revert, Export, Refresh, safe mode, inspector toggle.
//! - Pane 1 — Connections: saved connections with state dots, search,
//!   + Connection flow. Selecting a connection scopes pane 2.
//! - Pane 2 — Tables & objects: Items/History tabs, object search, grouped
//!   Tables/Views, schema footer.
//! - Pane 3 — Workspace: query editor above the data grid (fixed vertical
//!   split), grid action strip (Data/Structure, row ops, selection summary),
//!   optional console strip.
//! - Pane 4 — Row inspector (toggleable, off by default): Details/Assistant
//!   tabs, field search, editable field list.
//! - Status bar: connection state, query duration, staged changes, pager.

use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Editor, EditorState, Input, InputState};
use gpui_kit::component::table::TableState;
use gpui_kit::component::{h_flex, v_flex, Disableable, Icon, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::*;

use sqlpro_core::change::{PendingChanges, RowID};
use sqlpro_core::connection::{validate, ConnectionConfig};
use sqlpro_core::database_type::DatabaseType;
use sqlpro_core::history::{QueryHistoryEntry, QueryHistoryFilter, QueryHistoryStorage};
use sqlpro_core::result::QueryResult;
use sqlpro_core::schema::{ColumnInfo, TableInfo};
use sqlpro_core::sqlgen::{SqlStatementGenerator, DEFAULT_MARKER};
use sqlpro_core::value::CellValue;
use sqlpro_core::write::{
    engine_reports_real_counts, DataWriteExecutor, DataWritePlan, ParameterizedExecutor,
    TransactionOwner,
};

use crate::grid::{ResultsDelegate, RowVisual};
use crate::store;

actions!(workspace, [RunQuery, NewQuery, SaveChanges]);

const DEFAULT_ROW_LIMIT: usize = 500;
const HISTORY_PANEL_SIZE: usize = 30;

// Dark palette from ui_detail.md: near-black/charcoal canvas, subtle
// one-pixel separators, restrained accent.
const COLOR_BG: u32 = 0x1B1D21;
const COLOR_SURFACE: u32 = 0x202227;
const COLOR_SURFACE2: u32 = 0x25272C;
const COLOR_EDITOR: u32 = 0x14161A;
const COLOR_BORDER: u32 = 0x33363C;
const COLOR_BORDER_SOFT: u32 = 0x26292E;
const COLOR_TEXT: u32 = 0xE7E9EC;
const COLOR_MUTED: u32 = 0x8B909A;
const COLOR_ACCENT: u32 = 0x2F7CF6;
const COLOR_GREEN: u32 = 0x3FB950;
const COLOR_AMBER: u32 = 0xD29922;
const COLOR_RED: u32 = 0xF85149;
const COLOR_HOVER: u32 = 0x2B2E34;

/// Resolve a chrome color for the active theme: the constants are the dark
/// palette; light mode maps each to its light counterpart.
fn color(rgb_value: u32) -> Hsla {
    let resolved = if crate::theme::is_dark() {
        rgb_value
    } else {
        match rgb_value {
            COLOR_BG => 0xFFFFFF,
            COLOR_SURFACE => 0xF5F5F6,
            COLOR_SURFACE2 => 0xF7F7F8,
            COLOR_EDITOR => 0xFFFFFF,
            COLOR_BORDER => 0xE2E2E5,
            COLOR_BORDER_SOFT => 0xECECEE,
            COLOR_TEXT => 0x1D1D1F,
            COLOR_MUTED => 0x86868B,
            COLOR_GREEN => 0x28A745,
            COLOR_AMBER => 0xF0A020,
            COLOR_RED => 0xD9534F,
            COLOR_HOVER => 0xE8E8EA,
            // PK-badge background tint.
            0x3A2E14 => 0xFDF3D8,
            other => other,
        }
    };
    gpui::rgb(resolved).into()
}

/// State of the active connection (drives the status dot colors).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnState {
    Connected,
    Connecting,
    Offline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GridMode {
    Data,
    Structure,
}

/// Safety mode for risky write queries (ui_detail.md toolbar).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SafeMode {
    Off,
    Warn,
    Strict,
}

impl SafeMode {
    fn next(self) -> Self {
        match self {
            SafeMode::Off => SafeMode::Warn,
            SafeMode::Warn => SafeMode::Strict,
            SafeMode::Strict => SafeMode::Off,
        }
    }

    fn label(self) -> &'static str {
        match self {
            SafeMode::Off => "Off",
            SafeMode::Warn => "Warn",
            SafeMode::Strict => "Strict",
        }
    }

    fn color(self) -> Hsla {
        match self {
            SafeMode::Off => color(COLOR_MUTED),
            SafeMode::Warn => color(COLOR_AMBER),
            SafeMode::Strict => color(COLOR_GREEN),
        }
    }
}

/// Format for copying the selected row out of the inspector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowCopyFormat {
    Json,
    Yaml,
    Csv,
}

impl RowCopyFormat {
    fn label(self) -> &'static str {
        match self {
            RowCopyFormat::Json => "JSON",
            RowCopyFormat::Yaml => "YAML",
            RowCopyFormat::Csv => "CSV",
        }
    }
}

pub struct Workspace {
    connections: Vec<ConnectionConfig>,
    active: Option<Arc<ConnectionConfig>>,
    driver: Option<Arc<dyn ParameterizedExecutor>>,
    tables: Vec<TableInfo>,
    active_table: Option<usize>,
    status: String,
    running: bool,

    // Current result set (the grid's model).
    table_name: String,
    pk_columns: Vec<String>,
    columns: Vec<ColumnInfo>,
    /// The table's columns, cached for the inspector and structure view.
    structure: Vec<ColumnInfo>,
    /// The row as read from the server, kept for old-value tracking and
    /// keyless matching — TablePro's originalRow.
    original_rows: Vec<Vec<CellValue>>,
    /// What the grid displays: originals with staged edits applied, plus
    /// inserted rows.
    rows: Vec<Vec<CellValue>>,
    row_ids: Vec<RowID>,
    /// Server-reported row count for the pager ("1–500 of 1127").
    result_row_total: Option<usize>,
    /// Last executed query: (duration ms, row/affected count) for the
    /// status bar's result status.
    last_result: Option<(f64, usize)>,
    /// Error from the last failed query/save; stays visible in the footer
    /// until the next action resolves it.
    last_error: Option<String>,

    pending: PendingChanges,
    history: Option<QueryHistoryStorage>,
    history_entries: Vec<(String, String)>,
    console_log: Vec<(bool, String, String)>, // (ok, sql one-line, detail)

    // UI state
    show_inspector: bool,
    show_console: bool,
    show_form: bool,
    objects_tab: usize,   // 0 = Items, 1 = History
    inspector_tab: usize, // 0 = Details, 1 = Assistant
    grid_mode: GridMode,
    safe_mode: SafeMode,
    dark_mode: bool,
    conn_search: Entity<InputState>,
    objects_search: Entity<InputState>,
    inspector_search: Entity<InputState>,

    editor: Entity<EditorState>,
    table: Entity<TableState<ResultsDelegate>>,
    cell_input: Entity<InputState>,
    form: ConnectionForm,
}

struct ConnectionForm {
    host: Entity<InputState>,
    port: Entity<InputState>,
    user: Entity<InputState>,
    password: Entity<InputState>,
    database: Entity<InputState>,
}

impl Workspace {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let connections = store::load_connections();
        let editor =
            cx.new(|cx| EditorState::new(window, cx).placeholder("Type SQL… (⌘⏎ to run)"));
        let table = cx.new(|cx| {
            TableState::new(ResultsDelegate::empty(), window, cx)
                .col_resizable(true)
                .row_selectable(true)
                .cell_selectable(true)
        });
        let cell_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Selected cell value"));
        let conn_search =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search connections"));
        let objects_search = cx.new(|cx| InputState::new(window, cx).placeholder("Search objects"));
        let inspector_search = cx.new(|cx| InputState::new(window, cx).placeholder("Search fields"));
        let form = ConnectionForm {
            host: cx.new(|cx| InputState::new(window, cx).placeholder("host (localhost)")),
            port: cx.new(|cx| InputState::new(window, cx).placeholder("port")),
            user: cx.new(|cx| InputState::new(window, cx).placeholder("user")),
            password: cx.new(|cx| InputState::new(window, cx).placeholder("password")),
            database: cx.new(|cx| InputState::new(window, cx).placeholder("database")),
        };

        let history =
            QueryHistoryStorage::open(&sqlpro_core::history::default_history_path()).ok();
        let history_entries = history
            .as_ref()
            .map(|h| {
                h.fetch(&QueryHistoryFilter::default(), None, HISTORY_PANEL_SIZE)
                    .map(|p| p.entries.into_iter().map(|e| (e.id, e.query)).collect())
                    .unwrap_or_default()
            })
            .unwrap_or_default();

        let mut ws = Self {
            connections,
            active: None,
            driver: None,
            tables: Vec::new(),
            active_table: None,
            status: "Ready".to_string(),
            running: false,
            table_name: String::new(),
            pk_columns: Vec::new(),
            columns: Vec::new(),
            structure: Vec::new(),
            original_rows: Vec::new(),
            rows: Vec::new(),
            row_ids: Vec::new(),
            result_row_total: None,
            last_result: None,
            last_error: None,
            pending: PendingChanges::new(),
            history,
            history_entries,
            console_log: Vec::new(),
            show_inspector: false,
            show_console: false,
            show_form: false,
            objects_tab: 0,
            inspector_tab: 0,
            grid_mode: GridMode::Data,
            safe_mode: SafeMode::Warn,
            dark_mode: true,
            conn_search,
            objects_search,
            inspector_search,
            editor,
            table,
            cell_input,
            form,
        };

        ws.bind_keys(cx);

        // Optional auto-connect for a saved connection, e.g.
        // SQLPRO_CONNECT="dental_hub (local)". Without it, open the
        // in-memory demo database so a fresh install works with zero setup.
        match std::env::var("SQLPRO_CONNECT") {
            Ok(name) => match ws.connections.iter().position(|c| c.name == name) {
                Some(ix) => ws.connect_saved(ix, window, cx),
                None => {
                    ws.status = format!("No saved connection named \"{name}\"");
                    ws.connect_demo(window, cx);
                }
            },
            Err(_) => ws.connect_demo(window, cx),
        }

        ws
    }

    fn bind_keys(&self, cx: &mut Context<Self>) {
        let mod_key = if cfg!(target_os = "macos") { "cmd" } else { "ctrl" };
        cx.bind_keys([
            KeyBinding::new(&format!("{mod_key}-enter"), RunQuery, None),
            KeyBinding::new(&format!("{mod_key}-e"), NewQuery, None),
            KeyBinding::new(&format!("{mod_key}-t"), NewQuery, None),
            KeyBinding::new(&format!("{mod_key}-s"), SaveChanges, None),
        ]);
    }

    fn conn_state(&self) -> ConnState {
        if self.driver.is_some() {
            ConnState::Connected
        } else if self.running {
            ConnState::Connecting
        } else {
            ConnState::Offline
        }
    }

    fn state_dot(&self, state: ConnState) -> Div {
        let c = match state {
            ConnState::Connected => COLOR_GREEN,
            ConnState::Connecting => COLOR_AMBER,
            ConnState::Offline => COLOR_MUTED,
        };
        div().w(px(7.)).h(px(7.)).rounded_full().flex_shrink_0().bg(color(c))
    }

    // ---- Connections ---------------------------------------------------

    fn connect_demo(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut config = ConnectionConfig::new(
            "Demo (SQLite :memory:)",
            DatabaseType::new(DatabaseType::SQLITE),
        );
        config.path = Some(":memory:".into());
        let config = Arc::new(config);
        // Show the demo in the connections pane as a saved entry (not
        // persisted to disk).
        if !self.connections.iter().any(|c| c.name == config.name) {
            self.connections.push(config.as_ref().clone());
        }
        let seed: Option<std::sync::Arc<str>> = Some(std::sync::Arc::from(DEMO_SEED));
        self.connect(config, seed, window, cx);
    }

    fn connect_saved(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(config) = self.connections.get(ix).cloned() else { return };
        if self
            .active
            .as_ref()
            .is_some_and(|a| a.id == config.id && self.driver.is_some())
        {
            self.status = format!("Already connected: {}", config.name);
            cx.notify();
            return;
        }
        self.connect(Arc::new(config), None, window, cx);
    }

    fn connect_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let read = |e: &Entity<InputState>| e.read(cx).value().trim().to_string();
        let host = read(&self.form.host);
        let user = read(&self.form.user);
        let database = read(&self.form.database);
        let port: Option<u16> = read(&self.form.port).parse().ok();
        let password = read(&self.form.password);

        let mut config = ConnectionConfig::new(
            format!(
                "{}:{}/{}",
                if host.is_empty() { "localhost" } else { &host },
                port.unwrap_or(5432),
                database
            ),
            DatabaseType::new(DatabaseType::POSTGRESQL),
        );
        config.host = Some(if host.is_empty() { "localhost".into() } else { host });
        config.port = port;
        config.username = Some(if user.is_empty() { whoami_fallback() } else { user });
        config.password = Some(password);
        config.database = Some(database);

        if let Err(e) = validate(&config) {
            self.status = format!("Invalid connection: {e}");
            self.log_console(false, "connect", &e.to_string());
            cx.notify();
            return;
        }

        if !self.connections.iter().any(|c| c.id == config.id) {
            self.connections.push(config.clone());
            store::save_connections(&self.connections);
        }
        self.show_form = false;
        self.connect(Arc::new(config), None, window, cx);
    }

    fn connect(
        &mut self,
        config: Arc<ConnectionConfig>,
        seed: Option<std::sync::Arc<str>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.running {
            return;
        }
        self.running = true;
        self.status = format!("Connecting to {}…", config.name);
        self.last_error = None;
        cx.notify();

        cx.spawn_in(window, async move |this, cx| {
            let connect_config = Arc::clone(&config);
            let connect = sqlpro_drivers::runtime::on_thread(move || {
                sqlpro_drivers::create_driver(&connect_config)
            })
            .await;
            let outcome: std::result::Result<Arc<dyn ParameterizedExecutor>, String> =
                match connect {
                    Ok(driver) => Ok(Arc::from(driver)),
                    Err(e) => Err(e.to_string()),
                };

            if let (Ok(driver), Some(seed)) = (&outcome, &seed) {
                if let Err(e) = driver.execute(seed).await {
                    eprintln!("demo seed failed: {e}");
                }
            }

            let tables = match &outcome {
                Ok(driver) => driver.list_tables().await.unwrap_or_default(),
                Err(_) => Vec::new(),
            };

            _ = this.update(cx, |ws, cx| {
                ws.running = false;
                match outcome {
                    Ok(driver) => {
                        ws.log_console(
                            true,
                            &format!("connect {}", config.name),
                            &format!("{} tables", tables.len()),
                        );
                        ws.status = format!("Connected: {}", driver.label());
                        ws.driver = Some(driver);
                        ws.active = Some(config);
                        ws.tables = tables;
                    }
                    Err(e) => {
                        ws.log_console(false, &format!("connect {}", config.name), &e);
                        ws.status = format!("Connection failed: {e}");
                        ws.last_error = Some(e);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    // ---- Queries -------------------------------------------------------

    fn run_query(&mut self, _: &RunQuery, window: &mut Window, cx: &mut Context<Self>) {
        if self.running {
            return;
        }
        let Some(driver) = self.driver.clone() else {
            self.status = "Not connected".into();
            cx.notify();
            return;
        };
        let sql = self.editor.read(cx).value().to_string();
        if sql.trim().is_empty() {
            return;
        }

        self.running = true;
        self.status = "Running…".into();
        cx.notify();

        cx.spawn_in(window, async move |this, cx| {
            let started = std::time::Instant::now();
            let result = driver.execute(&sql).await;
            let elapsed = started.elapsed().as_secs_f64() * 1000.0;

            _ = this.update(cx, |ws, cx| {
                ws.running = false;
                let one_line: String =
                    sql.lines().map(|l| l.trim()).collect::<Vec<_>>().join(" ");
                let clipped: String = one_line.chars().take(120).collect();
                match result {
                    Ok(result) => {
                        let count = if result.is_query {
                            result.rows.len()
                        } else {
                            result.rows_affected as usize
                        };
                        ws.log_console(
                            true,
                            &clipped,
                            &format!("{count} rows, {elapsed:.0} ms"),
                        );
                        ws.status = format!(
                            "{} — {count} rows, {elapsed:.0} ms",
                            if result.is_query { "Query" } else { "Statement" },
                        );
                        ws.last_result = Some((elapsed, count));
                        ws.last_error = None;
                        ws.grid_mode = GridMode::Data;
                        ws.result_row_total = None;
                        ws.apply_result(result, cx);
                        ws.record_history(&sql, std::time::Duration::from_millis(elapsed as u64), true, None, cx);
                    }
                    Err(e) => {
                        ws.log_console(false, &clipped, &e.to_string());
                        ws.status = format!("Error: {e}");
                        ws.last_error = Some(e.to_string());
                        ws.record_history(&sql, std::time::Duration::from_millis(elapsed as u64), false, Some(&e.to_string()), cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn log_console(&mut self, ok: bool, sql: &str, detail: &str) {
        self.console_log
            .insert(0, (ok, sql.to_string(), detail.to_string()));
        self.console_log.truncate(200);
    }

    fn record_history(
        &mut self,
        sql: &str,
        elapsed: std::time::Duration,
        success: bool,
        error: Option<&str>,
        _cx: &mut Context<Self>,
    ) {
        let Some(connection) = self.active.clone() else { return };
        let mut entry =
            QueryHistoryEntry::new(sql, connection.id.to_string(), &connection.database_type);
        entry.execution_time = elapsed.as_secs_f64();
        entry.was_successful = success;
        entry.error_message = error.map(|e| e.to_string());
        entry.row_count = -1;
        if let Some(history) = &self.history {
            let _ = history.record(&entry);
        }
        self.history_entries
            .insert(0, (entry.id.clone(), entry.query.clone()));
        self.history_entries.truncate(HISTORY_PANEL_SIZE);
    }

    fn apply_result(&mut self, result: QueryResult, cx: &mut Context<Self>) {
        self.columns = result.columns.clone();
        self.original_rows = result.rows.clone();
        self.rows = result.rows.clone();
        self.row_ids = (0..self.rows.len() as i64).map(RowID::Existing).collect();
        self.pending.clear();

        self.table.update(cx, |table, cx| {
            *table.delegate_mut() = ResultsDelegate {
                columns: result.columns.iter().map(|c| c.name.clone()).collect(),
                rows: result.rows,
                visual: Vec::new(),
            };
            table.refresh(cx);
        });
    }

    fn open_table(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(table) = self.tables.get(ix) else { return };
        let sql = format!(
            "SELECT * FROM \"{}\" LIMIT {DEFAULT_ROW_LIMIT}",
            table.name.replace('"', "\"\"")
        );
        let sql_value = sql.clone();
        self.editor
            .update(cx, |editor, cx| editor.set_value(sql_value, window, cx));
        self.table_name = table.name.clone();
        self.active_table = Some(ix);
        self.grid_mode = GridMode::Data;

        // Cache the table's structure for the inspector and PK discovery.
        let table_for_columns = table.name.clone();
        let driver = self.driver.clone();
        let row_total = table.row_estimate;
        cx.spawn_in(window, async move |this, cx| {
            let (pk, structure) = if let Some(driver) = driver {
                let table = table_for_columns.clone();
                let columns = sqlpro_drivers::runtime::on_tokio(async move {
                    driver.list_columns(&table).await
                })
                .await
                .unwrap_or_default();
                let pk = columns
                    .iter()
                    .filter(|c| c.is_primary_key)
                    .map(|c| c.name.clone())
                    .collect::<Vec<String>>();
                (pk, columns)
            } else {
                (Vec::new(), Vec::new())
            };
            _ = this.update(cx, |ws, cx| {
                ws.pk_columns = pk;
                ws.structure = structure;
                ws.result_row_total = row_total.map(|n| n as usize);
                cx.notify();
            });
        })
        .detach();

        self.run_query(&RunQuery, window, cx);
    }

    fn new_query(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editor
            .update(cx, |editor, cx| editor.set_value("", window, cx));
        self.table_name.clear();
        self.active_table = None;
        self.structure.clear();
        self.pk_columns.clear();
        self.result_row_total = None;
        self.last_result = None;
        self.status = "New query — ready to run".into();
        cx.notify();
    }

    fn new_query_action(&mut self, _: &NewQuery, window: &mut Window, cx: &mut Context<Self>) {
        self.new_query(window, cx);
    }

    fn save_changes_action(
        &mut self,
        _: &SaveChanges,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.save_changes(window, cx);
    }

    // ---- Cell editing (the DataChangeManager pipeline) ------------------

    fn apply_cell_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let new_text = self.cell_input.read(cx).value().to_string();
        let Some((row_ix, col_ix)) = self.selected_cell(cx) else {
            self.status = "Select a cell first".into();
            cx.notify();
            return;
        };
        let Some(row_id) = self.row_ids.get(row_ix).copied() else { return };
        let Some(column) = self.columns.get(col_ix).cloned() else { return };
        let old_value = self
            .rows
            .get(row_ix)
            .and_then(|r| r.get(col_ix).cloned())
            .unwrap_or(CellValue::Null);
        let new_value = if new_text.is_empty() {
            CellValue::Null
        } else {
            CellValue::Text(new_text)
        };

        let original_row = self.original_rows.get(row_ix).cloned();
        let original_value = original_row.as_ref().and_then(|r| r.get(col_ix).cloned());

        let changed = self.pending.record_cell_change(
            row_id,
            col_ix,
            &column.name,
            original_value.clone().unwrap_or_else(|| old_value.clone()),
            new_value.clone(),
            original_row,
            Default::default(),
        );
        if changed {
            if let Some(row) = self.rows.get_mut(row_ix) {
                if let Some(cell) = row.get_mut(col_ix) {
                    *cell = new_value;
                }
            }
        }
        let _ = window;
        self.status = self.pending_status_text();
        self.refresh_grid(cx);
        cx.notify();
    }

    fn pending_status_text(&self) -> String {
        let edited: usize = self.pending.modified_cells.values().map(|s| s.len()).sum();
        format!(
            "{} edited · {} deleted · {} inserted",
            edited,
            self.pending.deleted_row_ids.len(),
            self.pending.inserted_row_ids.len()
        )
    }

    fn insert_row(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.columns.is_empty() {
            self.status = "Open a table first".into();
            cx.notify();
            return;
        }
        let row_id = RowID::Inserted(uuid::Uuid::new_v4());
        let values: Vec<CellValue> = self
            .columns
            .iter()
            .map(|_| CellValue::Text(DEFAULT_MARKER.to_string()))
            .collect();
        self.pending
            .record_row_insertion(row_id, values, Default::default());
        self.rows
            .push(vec![CellValue::Null; self.columns.len()]);
        self.row_ids.push(row_id);
        self.status = self.pending_status_text();
        self.refresh_grid(cx);
        cx.notify();
    }

    fn delete_row(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(row_ix) = self.selected_row(cx) else {
            self.status = "Select a row first".into();
            cx.notify();
            return;
        };
        let Some(row_id) = self.row_ids.get(row_ix).copied() else { return };

        if row_id.is_inserted() {
            self.pending.undo_row_insertion(row_id);
            self.rows.remove(row_ix);
            self.row_ids.remove(row_ix);
        } else {
            let original_row = self.original_rows.get(row_ix).cloned().unwrap_or_default();
            self.pending
                .record_row_deletion(row_id, original_row, Default::default());
        }
        self.status = self.pending_status_text();
        self.refresh_grid(cx);
        cx.notify();
    }

    fn save_changes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pending.is_empty() {
            self.status = "Nothing to save".into();
            cx.notify();
            return;
        }
        let Some(driver) = self.driver.clone() else { return };
        let has_pk = !self.pk_columns.is_empty();
        let columns = self.columns.iter().map(|c| c.name.clone()).collect();
        let pk = self.pk_columns.clone();
        let table_name = self.table_name.clone();
        let snapshot = self.pending.snapshot();
        let engine = self
            .active
            .as_ref()
            .map(|c| c.database_type.clone())
            .unwrap_or_else(|| DatabaseType::new(DatabaseType::SQLITE));

        self.status = "Saving…".into();
        cx.notify();

        cx.spawn_in(window, async move |this, cx| {
            let real_counts = engine_reports_real_counts(&engine);
            let outcome = sqlpro_drivers::runtime::on_tokio(async move {
                let generator = SqlStatementGenerator::new(
                    table_name,
                    None,
                    columns,
                    pk,
                    engine.clone(),
                    Default::default(),
                    Default::default(),
                    None,
                    false,
                    None,
                );
                let attributed = generator.generate_attributed_statements(
                    &snapshot.changes,
                    &snapshot.inserted_row_data,
                    &snapshot.deleted_row_ids,
                    &snapshot.inserted_row_ids,
                );
                let plan =
                    DataWritePlan::from_attributed(attributed, has_pk, TransactionOwner::App);
                let executor = DataWriteExecutor { driver: driver.as_ref(), real_counts };
                executor.run(&plan).await
            })
            .await;

            _ = this.update_in(cx, |ws, window, cx| {
                match outcome {
                    Ok(result) => {
                        let affected: usize =
                            result.executed.iter().map(|s| s.rows_affected as usize).sum();
                        ws.log_console(
                            true,
                            "commit",
                            &format!(
                                "{} statement(s), {affected} row(s) affected",
                                result.executed.len()
                            ),
                        );
                        ws.status = format!(
                            "Saved: {} statement(s), {affected} row(s) affected",
                            result.executed.len()
                        );
                        ws.last_error = None;
                        ws.pending.clear();
                        ws.reload_current_table(window, cx);
                    }
                    Err(e) => {
                        ws.log_console(false, "commit", &e.to_string());
                        ws.status = format!("Save failed: {e}");
                        ws.last_error = Some(e.to_string());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn reload_current_table(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.table_name.is_empty() {
            self.rows = self.original_rows.clone();
            self.row_ids = (0..self.rows.len() as i64).map(RowID::Existing).collect();
            self.refresh_grid(cx);
            return;
        }
        let sql = format!(
            "SELECT * FROM \"{}\" LIMIT {DEFAULT_ROW_LIMIT}",
            self.table_name.replace('"', "\"\"")
        );
        self.editor
            .update(cx, |editor, cx| editor.set_value(sql, window, cx));
        self.run_query(&RunQuery, window, cx);
    }

    fn revert_changes(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.pending.clear();
        let original_len = self.original_rows.len();
        self.rows.truncate(original_len);
        for (ix, original) in self.original_rows.iter().enumerate() {
            if let Some(row) = self.rows.get_mut(ix) {
                *row = original.clone();
            }
        }
        self.row_ids = (0..original_len as i64).map(RowID::Existing).collect();
        self.refresh_grid(cx);
        self.status = "Reverted all staged changes".into();
        cx.notify();
    }

    // ---- Export --------------------------------------------------------

    fn export_csv(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.rows.is_empty() {
            self.status = "No rows to export".into();
            cx.notify();
            return;
        }
        let options = sqlpro_core::csv::CsvFieldOptions::default();
        let mut out = String::new();
        let header: Vec<String> = self
            .columns
            .iter()
            .map(|c| sqlpro_core::csv::csv_field(&c.name, ',', &options))
            .collect();
        out.push_str(&header.join(","));
        out.push('\n');
        for row in &self.rows {
            let fields: Vec<String> = row
                .iter()
                .map(|c| sqlpro_core::csv::csv_field(&c.display(), ',', &options))
                .collect();
            out.push_str(&fields.join(","));
            out.push('\n');
        }
        let path = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_default()
            .join("sqlpro-export.csv");
        match std::fs::write(&path, out) {
            Ok(_) => {
                self.status =
                    format!("Exported {} rows to {}", self.rows.len(), path.display());
            }
            Err(e) => {
                self.status = format!("Export failed: {e}");
            }
        }
        cx.notify();
    }

    // ---- Row copying (inspector) ---------------------------------------

    /// Copy only the currently selected row to the clipboard in the given
    /// format (JSON object, YAML document, or CSV with a header line).
    fn copy_row_as(&mut self, format: RowCopyFormat, cx: &mut Context<Self>) {
        let Some(row_ix) = self.selected_row(cx) else {
            self.status = "No row selected — select a row to copy".into();
            cx.notify();
            return;
        };
        let Some(row) = self.rows.get(row_ix).cloned() else { return };

        let text = match format {
            RowCopyFormat::Json => self.row_as_json(&row),
            RowCopyFormat::Yaml => self.row_as_yaml(&row),
            RowCopyFormat::Csv => self.row_as_csv(&row),
        };
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
        self.status = format!("Row {} copied as {}", row_ix + 1, format.label());
        cx.notify();
    }

    fn row_as_json(&self, row: &[CellValue]) -> String {
        let mut object = serde_json::Map::new();
        for (ix, column) in self.columns.iter().enumerate() {
            let value = match row.get(ix) {
                None | Some(CellValue::Null) => serde_json::Value::Null,
                Some(CellValue::Bytes(_)) => serde_json::Value::String("<binary>".into()),
                Some(CellValue::Text(text)) => {
                    // Preserve numeric/boolean types when the value parses
                    // cleanly; everything else stays a string.
                    if text == "true" {
                        serde_json::Value::Bool(true)
                    } else if text == "false" {
                        serde_json::Value::Bool(false)
                    } else if let Ok(n) = text.parse::<i64>() {
                        serde_json::Value::Number(n.into())
                    } else if let Ok(f) = text.parse::<f64>() {
                        serde_json::Number::from_f64(f)
                            .map(serde_json::Value::Number)
                            .unwrap_or_else(|| serde_json::Value::String(text.clone()))
                    } else {
                        serde_json::Value::String(text.clone())
                    }
                }
            };
            object.insert(column.name.clone(), value);
        }
        serde_json::Value::Object(object).to_string()
    }

    fn row_as_yaml(&self, row: &[CellValue]) -> String {
        let mut out = String::new();
        for (ix, column) in self.columns.iter().enumerate() {
            out.push_str(&yaml_quoted_key(&column.name));
            out.push(':');
            match row.get(ix) {
                None | Some(CellValue::Null) => out.push_str(" null"),
                Some(CellValue::Bytes(_)) => out.push_str(" \"<binary>\""),
                Some(CellValue::Text(text)) => {
                    out.push(' ');
                    out.push_str(&yaml_double_quoted(text));
                }
            }
            out.push('\n');
        }
        out
    }

    fn row_as_csv(&self, row: &[CellValue]) -> String {
        let options = sqlpro_core::csv::CsvFieldOptions::default();
        let header: Vec<String> = self
            .columns
            .iter()
            .map(|c| sqlpro_core::csv::csv_field(&c.name, ',', &options))
            .collect();
        let values: Vec<String> = row
            .iter()
            .map(|c| sqlpro_core::csv::csv_field(&c.display(), ',', &options))
            .collect();
        format!("{},{}\n", header.join(","), values.join(","))
    }

    // ---- Helpers -------------------------------------------------------

    fn selected_cell(&self, cx: &App) -> Option<(usize, usize)> {
        match self.table.read(cx).selection() {
            gpui_kit::component::table::TableSelection::Cell(r, c) => Some((r, c)),
            gpui_kit::component::table::TableSelection::Row(r) => Some((r, 0)),
            _ => None,
        }
    }

    fn selected_row(&self, cx: &App) -> Option<usize> {
        match self.table.read(cx).selection() {
            gpui_kit::component::table::TableSelection::Cell(r, _) => Some(r),
            gpui_kit::component::table::TableSelection::Row(r) => Some(r),
            _ => None,
        }
    }

    fn refresh_grid(&mut self, cx: &mut Context<Self>) {
        let columns = self.columns.iter().map(|c| c.name.clone()).collect();
        let rows = self.rows.clone();
        let mut visual = vec![RowVisual::Normal; self.rows.len()];
        for (ix, row_id) in self.row_ids.iter().enumerate() {
            if self.pending.is_row_deleted(*row_id) {
                visual[ix] = RowVisual::Deleted;
            } else if self.pending.is_row_inserted(*row_id) {
                visual[ix] = RowVisual::Inserted;
            } else if !self.pending.modified_columns(*row_id).is_empty() {
                visual[ix] = RowVisual::Modified;
            }
        }
        self.table.update(cx, |table, cx| {
            *table.delegate_mut() = ResultsDelegate { columns, rows, visual };
            table.refresh(cx);
        });
    }

    fn icon_button(
        &self,
        id: &'static str,
        icon: IconName,
        active: bool,
    ) -> Button {
        Button::new(id)
            .ghost()
            .small()
            .when(active, |b| b.primary())
            .child(Icon::new(icon))
    }

    /// A toolbar button rendered with a unicode glyph instead of an SVG icon.
    fn glyph_button(&self, id: &'static str, glyph: &'static str, active: bool) -> Button {
        Button::new(id)
            .ghost()
            .small()
            .when(active, |b| b.primary())
            .child(
                div()
                    .text_size(px(15.))
                    .line_height(px(1.))
                    .child(glyph),
            )
    }

    fn engine_label(db: &DatabaseType) -> &'static str {
        match db.driver_id() {
            "postgresql" => "PostgreSQL",
            "mysql" => "MySQL",
            "sqlite" => "SQLite",
            other => match other {
                _ => "Database",
            },
        }
    }

    fn connection_summary(config: &ConnectionConfig) -> String {
        match (&config.host, &config.database) {
            (Some(host), Some(db)) => format!("{host}:{}·{db}", config.port.unwrap_or(5432)),
            (Some(host), None) => host.clone(),
            (None, Some(path)) => path.clone(),
            _ => String::new(),
        }
    }

    fn filtered_connections(&self, cx: &App) -> Vec<(usize, &ConnectionConfig)> {
        let query = self.conn_search.read(cx).value().trim().to_lowercase();
        self.connections
            .iter()
            .enumerate()
            .filter(|(_, c)| {
                query.is_empty()
                    || c.name.to_lowercase().contains(&query)
                    || Self::connection_summary(c).to_lowercase().contains(&query)
                    || Self::engine_label(&c.database_type).to_lowercase().contains(&query)
            })
            .collect()
    }

    fn filtered_tables(&self, cx: &App) -> Vec<(usize, &TableInfo)> {
        let query = self.objects_search.read(cx).value().trim().to_lowercase();
        self.tables
            .iter()
            .enumerate()
            .filter(|(_, t)| query.is_empty() || t.name.to_lowercase().contains(&query))
            .collect()
    }
}

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let border = color(COLOR_BORDER);
        let pending_count = self.pending.changes.len();
        let shown = self.rows.len();
        let total = self.result_row_total.unwrap_or(shown);
        let conn_state = self.conn_state();

        v_flex()
            .id("workspace")
            .size_full()
            .bg(color(COLOR_BG))
            .text_color(color(COLOR_TEXT))
            .text_size(px(13.))
            .on_action(cx.listener(Self::run_query))
            .on_action(cx.listener(Self::new_query_action))
            .on_action(cx.listener(Self::save_changes_action))
            // ---- Application toolbar ----
            .child(self.render_toolbar(cx))
            // ---- Body: the four panes (navigators fixed, inspector optional) ----
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_connections_pane(cx))
                    .child(self.render_objects_pane(cx))
                    .child(self.render_workspace_pane(cx))
                    .when(self.show_inspector, |this| {
                        this.child(self.render_inspector(cx))
                    }),
            )
            // ---- Status bar ----
            .child(
                h_flex()
                    .w_full()
                    .h(px(26.))
                    .flex_shrink_0()
                    .items_center()
                    .gap_3()
                    .px_3()
                    .border_t_1()
                    .border_color(border)
                    .bg(color(COLOR_SURFACE))
                    .text_size(px(11.))
                    .text_color(color(COLOR_MUTED))
                    // Connection state + engine
                    .child(
                        h_flex()
                            .gap_1p5()
                            .items_center()
                            .child(self.state_dot(conn_state))
                            .child(
                                div().child(self.active.as_ref().map(|c| {
                                    Self::engine_label(&c.database_type).to_string()
                                }).unwrap_or_else(|| "No connection".into())),
                            ),
                    )
                    .child(div().w(px(1.)).h(px(12.)).bg(color(COLOR_BORDER_SOFT)))
                    // Result status: duration + counts
                    .when_some(self.last_result, |bar, (ms, count)| {
                        bar.child(div().child(format!("{ms:.0} ms · {count} rows")))
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .when_some(self.last_error.clone(), |this, e| {
                                this.text_color(color(COLOR_RED)).child(format!("Error: {e}"))
                            })
                            .when(self.last_error.is_none(), |this| {
                                when_pending(this, pending_count, &self.status)
                            }),
                    )
                    .when(pending_count > 0, |bar| {
                        bar.child(
                            h_flex()
                                .gap_1()
                                .items_center()
                                .child(div().w(px(7.)).h(px(7.)).rounded_full().bg(color(COLOR_AMBER)))
                                .child(
                                    div()
                                        .text_color(color(COLOR_AMBER))
                                        .child(format!("{pending_count} staged")),
                                ),
                        )
                    })
                    .child(div().child(format!("1–{shown} of {total} rows")))
                    // Light/dark theme toggle
                    .child(
                        Button::new("theme-toggle")
                            .ghost()
                            .xsmall()
                            .child(
                                h_flex()
                                    .gap_1()
                                    .child(
                                        div()
                                            .text_size(px(12.))
                                            .line_height(px(1.))
                                            .child(if self.dark_mode { "☀" } else { "☾" }),
                                    )
                                    .child(if self.dark_mode { "Light" } else { "Dark" }),
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.dark_mode = !this.dark_mode;
                                crate::theme::apply(this.dark_mode, cx);
                                this.status = if this.dark_mode {
                                    "Dark theme"
                                } else {
                                    "Light theme"
                                }
                                .into();
                                cx.notify();
                            })),
                    ),
            )
    }
}

/// Footer middle segment: staged count first, else the plain status text.
fn when_pending(this: Div, pending_count: usize, status: &str) -> Div {
    if pending_count > 0 {
        this.text_color(color(COLOR_TEXT)).child(status.to_string())
    } else {
        this.child(status.to_string())
    }
}

impl Workspace {
    // ---- Toolbar ----

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let border = color(COLOR_BORDER);
        let pending_count = self.pending.changes.len();
        let conn_state = self.conn_state();

        h_flex()
            .w_full()
            .h(px(48.))
            .flex_shrink_0()
            .items_center()
            .gap_1()
            .px_2()
            .bg(color(COLOR_SURFACE2))
            .border_b_1()
            .border_color(border)
            // Connection pill: state dot + engine + name + database
            .child(
                h_flex()
                    .gap_1p5()
                    .items_center()
                    .px_2()
                    .py(px(3.))
                    .rounded(px(6.))
                    .bg(color(COLOR_SURFACE))
                    .border_1()
                    .border_color(border)
                    .child(self.state_dot(conn_state))
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(color(COLOR_MUTED))
                            .child(self.active.as_ref().map(|c| {
                                Self::engine_label(&c.database_type).to_string()
                            }).unwrap_or_else(|| "—".into())),
                    )
                    .child(
                        div()
                            .max_w(px(180.))
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_size(px(12.))
                            .font_weight(FontWeight::MEDIUM)
                            .child(
                                self.active
                                    .as_ref()
                                    .map(|c| c.name.clone())
                                    .unwrap_or_else(|| "No connection".into()),
                            ),
                    )
                    .when_some(
                        self.active.as_ref().and_then(|c| c.database.clone()),
                        |pill, db| {
                            pill.child(
                                div()
                                    .text_size(px(10.5))
                                    .text_color(color(COLOR_MUTED))
                                    .child(db),
                            )
                        },
                    ),
            )
            .child(div().w(px(1.)).h(px(22.)).bg(border).mx_1())
            // Refresh
            .child(
                self.icon_button("refresh", IconName::RefreshCw, false).on_click(
                    cx.listener(|this, _, window, cx| {
                        this.reload_current_table(window, cx);
                    }),
                ),
            )
            .child(div().w(px(1.)).h(px(22.)).bg(border).mx_1())
            // Save (text label, disabled with nothing staged)
            .child(
                Button::new("commit")
                    .small()
                    .outline()
                    .disabled(pending_count == 0)
                    .child(
                        h_flex()
                            .gap_1()
                            .child(Icon::new(IconName::Save).xsmall())
                            .child("Save"),
                    )
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.save_changes(window, cx)
                    })),
            )
            // Revert (text label)
            .child(
                Button::new("revert")
                    .ghost()
                    .small()
                    .disabled(pending_count == 0)
                    .child(
                        h_flex()
                            .gap_1()
                            .child(Icon::new(IconName::Undo2).xsmall())
                            .child("Revert"),
                    )
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.revert_changes(window, cx)
                    })),
            )
            // Export
            .child(
                Button::new("export")
                    .ghost()
                    .small()
                    .child(
                        h_flex()
                            .gap_1()
                            .child(Icon::new(IconName::Download).xsmall())
                            .child("Export"),
                    )
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.export_csv(window, cx)
                    })),
            )
            .child(div().w(px(1.)).h(px(22.)).bg(border).mx_1())
            // Safe mode pill: cycles Off → Warn → Strict
            .child(
                Button::new("safe-mode")
                    .ghost()
                    .small()
                    .child(
                        h_flex()
                            .gap_1()
                            .child(
                                Icon::new(match self.safe_mode {
                                    SafeMode::Off => IconName::ShieldOff,
                                    _ => IconName::ShieldCheck,
                                })
                                .xsmall()
                                .text_color(self.safe_mode.color()),
                            )
                            .child(
                                div()
                                    .text_color(self.safe_mode.color())
                                    .child(format!("Safe: {}", self.safe_mode.label())),
                            ),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.safe_mode = this.safe_mode.next();
                        this.status =
                            format!("Safe mode: {}", this.safe_mode.label());
                        cx.notify();
                    })),
            )
            .child(div().flex_1())
            // Console toggle
            .child(
                self.icon_button("toggle-console", IconName::SquareTerminal, self.show_console)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.show_console = !this.show_console;
                        cx.notify();
                    })),
            )
            // Inspector toggle (pane 4) — "◨" is a square with the right
            // half filled, reading as "right-side panel".
            .child(
                self.glyph_button("toggle-inspector", "◨", self.show_inspector)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.show_inspector = !this.show_inspector;
                        cx.notify();
                    })),
            )
    }

    // ---- Pane 1: connections ----

    fn render_connections_pane(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let border = color(COLOR_BORDER);
        let connections = self.filtered_connections(cx);

        v_flex()
            .w(px(224.))
            .flex_shrink_0()
            .min_w_0()
            .h_full()
            .bg(color(COLOR_SURFACE))
            .border_r_1()
            .border_color(border)
            .child(
                h_flex()
                    .h(px(32.))
                    .px_3()
                    .items_center()
                    .justify_between()
                    .text_size(px(10.5))
                    .text_color(color(COLOR_MUTED))
                    .child("CONNECTIONS")
                    .child(
                        Button::new("add-conn")
                            .ghost()
                            .xsmall()
                            .child(Icon::new(IconName::Plus))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.show_form = !this.show_form;
                                cx.notify();
                            })),
                    ),
            )
            .child(div().px_2().pb_2().child(Input::new(&self.conn_search).small()))
            .child(
                v_flex()
                    .id("connection-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .children(connections.into_iter().map(|(ix, config)| {
                        let is_active = self
                            .active
                            .as_ref()
                            .is_some_and(|a| a.id == config.id && self.driver.is_some());
                        let summary = Self::connection_summary(config);
                        div()
                            .id(SharedString::from(format!("conn-{ix}")))
                            .flex()
                            .flex_shrink_0()
                            .items_center()
                            .gap_2()
                            .px_2()
                            .py(px(5.))
                            .mx_1()
                            .mb(px(1.))
                            .rounded(px(5.))
                            .overflow_hidden()
                            .when(is_active, |this| this.bg(color(COLOR_HOVER)))
                            .hover(|this| this.bg(color(COLOR_HOVER)))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.connect_saved(ix, window, cx)
                            }))
                            .child(self.state_dot(if is_active {
                                ConnState::Connected
                            } else {
                                ConnState::Offline
                            }))
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap_0p5()
                                    .child(
                                        div()
                                            .overflow_hidden()
                                            .whitespace_nowrap()
                                            .text_size(px(12.5))
                                            .font_weight(FontWeight::MEDIUM)
                                            .child(config.name.clone()),
                                    )
                                    .child(
                                        div()
                                            .overflow_hidden()
                                            .whitespace_nowrap()
                                            .text_size(px(10.5))
                                            .text_color(color(COLOR_MUTED))
                                            .child(summary),
                                    ),
                            )
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .text_size(px(9.5))
                                    .text_color(color(COLOR_MUTED))
                                    .child(
                                        match config.database_type.driver_id() {
                                            "postgresql" => "PG",
                                            "mysql" => "MY",
                                            "sqlite" => "LITE",
                                            _ => "DB",
                                        },
                                    ),
                            )
                    })),
            )
            .when(self.show_form, |this| this.child(self.render_connection_form(cx)))
    }

    // ---- Pane 2: tables & objects ----

    fn render_objects_pane(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let border = color(COLOR_BORDER);
        let tables = self.filtered_tables(cx);
        let table_count = tables.iter().filter(|(_, t)| !t.is_view).count();
        let view_count = tables.iter().filter(|(_, t)| t.is_view).count();

        v_flex()
            .w(px(252.))
            .flex_shrink_0()
            .min_w_0()
            .h_full()
            .bg(color(COLOR_SURFACE))
            .border_r_1()
            .border_color(border)
            // Items / History tabs
            .child(
                h_flex()
                    .h(px(32.))
                    .px_2()
                    .gap_3()
                    .items_center()
                    .border_b_1()
                    .border_color(color(COLOR_BORDER_SOFT))
                    .child(self.objects_tab_button("tab-items", "Items", 0, cx))
                    .child(self.objects_tab_button("tab-history", "History", 1, cx)),
            )
            .child(
                h_flex()
                    .px_2()
                    .py_2()
                    .child(Input::new(&self.objects_search).small()),
            )
            .when(self.objects_tab == 0, |this| {
                this.child(
                    v_flex()
                        .id("object-list")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        // Tables group
                        .child(
                            h_flex()
                                .px_3()
                                .py_1()
                                .gap_1()
                                .text_size(px(10.))
                                .text_color(color(COLOR_MUTED))
                                .child("TABLES")
                                .child(format!("({table_count})")),
                        )
                        .children(tables.iter().filter(|(_, t)| !t.is_view).map(|(ix, table)| {
                            self.object_row(ix, table, cx)
                        }))
                        // Views group
                        .when(view_count > 0, |this| {
                            this.child(
                                h_flex()
                                    .px_3()
                                    .py_1()
                                    .gap_1()
                                    .text_size(px(10.))
                                    .text_color(color(COLOR_MUTED))
                                    .child("VIEWS")
                                    .child(format!("({view_count})")),
                            )
                            .children(
                                tables.iter().filter(|(_, t)| t.is_view).map(|(ix, table)| {
                                    self.object_row(ix, table, cx)
                                }),
                            )
                        })
                        // Empty state: no compatible objects found
                        .when(self.tables.is_empty(), |this| {
                            this.child(
                                v_flex()
                                    .items_center()
                                    .gap_1()
                                    .p_4()
                                    .text_size(px(11.5))
                                    .text_color(color(COLOR_MUTED))
                                    .child(
                                        Icon::new(IconName::Database).text_color(color(COLOR_BORDER)),
                                    )
                                    .child(if self.driver.is_some() {
                                        "No tables found in this database"
                                    } else {
                                        "Connect to a database to browse its objects"
                                    }),
                            )
                        }),
                )
            })
            .when(self.objects_tab == 1, |this| {
                this.child(
                    v_flex()
                        .id("history-list")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .children(self.history_entries.iter().enumerate().map(|(ix, (_, query))| {
                            let one_line = query.lines().next().unwrap_or("").to_string();
                            let truncated: String = one_line.chars().take(44).collect();
                            div()
                                .id(SharedString::from(format!("history-{ix}")))
                                .flex()
                                .h(px(26.))
                                .flex_shrink_0()
                                .items_center()
                                .gap_2()
                                .px_2()
                                .mx_1()
                                .rounded(px(4.))
                                .overflow_hidden()
                                .text_size(px(11.5))
                                .text_color(color(COLOR_MUTED))
                                .hover(|this| this.bg(color(COLOR_HOVER)))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.load_history(ix, window, cx)
                                }))
                                .child(
                                    Icon::new(IconName::SquareTerminal)
                                        .xsmall()
                                        .flex_shrink_0(),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .child(truncated),
                                )
                        }))
                        .when(self.history_entries.is_empty(), |this| {
                            this.child(
                                div()
                                    .p_4()
                                    .text_size(px(11.5))
                                    .text_color(color(COLOR_MUTED))
                                    .child("Executed queries will appear here"),
                            )
                        }),
                )
            })
            // Object-pane footer: schema + create
            .child(
                h_flex()
                    .h(px(28.))
                    .flex_shrink_0()
                    .items_center()
                    .px_3()
                    .gap_2()
                    .border_t_1()
                    .border_color(border)
                    .child(
                        h_flex()
                            .gap_1()
                            .items_center()
                            .min_w_0()
                            .child(Icon::new(IconName::Layers).xsmall().text_color(color(COLOR_MUTED)))
                            .child(
                                div()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_size(px(11.))
                                    .text_color(color(COLOR_MUTED))
                                    .child(
                                        self.active
                                            .as_ref()
                                            .and_then(|c| c.database.clone())
                                            .unwrap_or_else(|| "no schema".into()),
                                    ),
                            ),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("add-table")
                            .ghost()
                            .xsmall()
                            .child(Icon::new(IconName::Table2))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.new_table_template(window, cx)
                            })),
                    ),
            )
    }

    fn objects_tab_button(
        &self,
        id: &'static str,
        label: &'static str,
        tab: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let active = self.objects_tab == tab;
        div()
            .id(id)
            .h_full()
            .flex()
            .items_center()
            .px_1()
            .text_size(px(12.))
            .font_weight(if active { FontWeight::MEDIUM } else { FontWeight::NORMAL })
            .text_color(if active { color(COLOR_TEXT) } else { color(COLOR_MUTED) })
            .when(active, |this| this.border_b_2().border_color(color(COLOR_ACCENT)))
            .hover(|this| this.text_color(color(COLOR_TEXT)))
            .child(label)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.objects_tab = tab;
                cx.notify();
            }))
    }

    fn object_row(&self, ix: &usize, table: &TableInfo, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = self.table_name == table.name;
        let count = table.row_estimate.map(|n| n.to_string()).unwrap_or_default();
        let ix = *ix;
        div()
            .id(SharedString::from(format!("object-{ix}")))
            .flex()
            .h(px(26.))
            .flex_shrink_0()
            .items_center()
            .gap_2()
            .px_2()
            .mx_1()
            .mb(px(1.))
            .rounded(px(4.))
            .overflow_hidden()
            .text_size(px(12.5))
            .when(selected, |this| {
                this.bg(color(COLOR_ACCENT)).text_color(gpui::white())
            })
            .hover(|this| this.bg(color(COLOR_HOVER)))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.open_table(ix, window, cx)
            }))
            .child(
                Icon::new(if table.is_view {
                    IconName::Eye
                } else {
                    IconName::Table2
                })
                .xsmall()
                .flex_shrink_0()
                .text_color(if selected {
                    gpui::white()
                } else {
                    color(COLOR_MUTED)
                }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(table.name.clone()),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .text_size(px(10.))
                    .text_color(if selected {
                        gpui::white()
                    } else {
                        color(COLOR_MUTED)
                    })
                    .child(count),
            )
    }

    // ---- Pane 3: query + data workspace ----

    fn render_workspace_pane(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        // h_full is load-bearing: without a definite height on this column,
        // every flex_1 child below collapses to content height.
        v_flex()
            .flex_1()
            .h_full()
            .min_w_0()
            .min_h_0()
            .overflow_hidden()
            .child(self.render_editor_pane(cx))
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .overflow_hidden()
                    .child(self.render_grid_area(cx))
                    .child(self.render_grid_action_strip(cx)),
            )
            .when(self.show_console, |this| this.child(self.render_console(cx)))
    }

    fn render_editor_pane(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let border = color(COLOR_BORDER);
        let dirty = !self
            .editor
            .read(cx)
            .value()
            .trim()
            .is_empty();

        v_flex()
            .w_full()
            .h(px(220.))
            .flex_shrink_0()
            .bg(color(COLOR_EDITOR))
            .border_b_1()
            .border_color(border)
            .child(
                h_flex()
                    .w_full()
                    .h(px(28.))
                    .flex_shrink_0()
                    .items_center()
                    .px_2()
                    .gap_2()
                    .border_b_1()
                    .border_color(color(COLOR_BORDER_SOFT))
                    .bg(color(COLOR_SURFACE))
                    // Query tab with dirty marker
                    .child(
                        h_flex()
                            .h(px(20.))
                            .items_center()
                            .gap_1p5()
                            .px_2()
                            .rounded(px(4.))
                            .bg(color(COLOR_SURFACE2))
                            .border_1()
                            .border_color(border)
                            .text_size(px(11.))
                            .child(
                                Icon::new(IconName::SquareTerminal)
                                    .xsmall()
                                    .text_color(color(COLOR_MUTED)),
                            )
                            .child("query-1")
                            .when(dirty, |this| {
                                this.child(
                                    div()
                                        .w(px(6.))
                                        .h(px(6.))
                                        .rounded_full()
                                        .bg(color(COLOR_AMBER)),
                                )
                            }),
                    )
                    .child(div().flex_1())
                    // Query actions live in the query panel header.
                    .child(
                        Button::new("new-sql")
                            .ghost()
                            .small()
                            .label("+ SQL")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.new_query(window, cx)
                            })),
                    )
                    .child(
                        Button::new("run")
                            .primary()
                            .small()
                            .disabled(self.running || self.driver.is_none())
                            .child(
                                h_flex()
                                    .gap_1()
                                    .child(Icon::new(IconName::Play).xsmall())
                                    .child("Run ⌘⏎"),
                            )
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.run_query(&RunQuery, window, cx)
                            })),
                    )
                    .child(
                        Button::new("run-all")
                            .small()
                            .outline()
                            .disabled(self.running || self.driver.is_none())
                            .label("Run all")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.run_query(&RunQuery, window, cx)
                            })),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .p_1()
                    .child(Editor::new(&self.editor).h(px(184.))),
            )
    }

    fn render_grid_area(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        match self.grid_mode {
            GridMode::Data => {
                if self.columns.is_empty() {
                    // Empty state instead of a blank grid.
                    v_flex()
                        .flex_1()
                        .min_h_0()
                        .items_center()
                        .justify_center()
                        .gap_2()
                        .bg(color(COLOR_BG))
                        .child(
                            Icon::new(IconName::Table)
                                .text_color(color(COLOR_BORDER))
                                .size(px(28.)),
                        )
                        .child(
                            div()
                                .text_size(px(12.5))
                                .text_color(color(COLOR_MUTED))
                                .child("Select a table or run a query to see results"),
                        )
                        .into_any_element()
                } else {
                    div()
                        .flex_1()
                        .min_h_0()
                        .overflow_hidden()
                        .bg(color(COLOR_BG))
                        .child(self.table.clone())
                        .into_any_element()
                }
            }
            GridMode::Structure => self.render_structure_view(cx).into_any_element(),
        }
    }

    fn render_structure_view(&self, _cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("structure-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .bg(color(COLOR_BG))
            .when(self.structure.is_empty(), |this| {
                this.child(
                    v_flex()
                        .flex_1()
                        .items_center()
                        .justify_center()
                        .gap_2()
                        .child(
                            Icon::new(IconName::TableProperties)
                                .text_color(color(COLOR_BORDER))
                                .size(px(28.)),
                        )
                        .child(
                            div()
                                .text_size(px(12.5))
                                .text_color(color(COLOR_MUTED))
                                .child("Open a table to view its structure"),
                        ),
                )
            })
            .children(self.structure.iter().map(|column| {
                h_flex()
                    .h(px(28.))
                    .flex_shrink_0()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .border_b_1()
                    .border_color(color(COLOR_BORDER_SOFT))
                    .text_size(px(12.))
                    .child(
                        div()
                            .w(px(200.))
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .font_weight(FontWeight::MEDIUM)
                            .child(column.name.clone()),
                    )
                    .child(
                        div()
                            .text_color(color(COLOR_MUTED))
                            .child(column.data_type.clone()),
                    )
                    .when(column.is_primary_key, |this| {
                        this.child(
                            div()
                                .px_1p5()
                                .py(px(1.))
                                .rounded(px(3.))
                                .text_size(px(9.5))
                                .bg(color(0x3A2E14))
                                .text_color(color(COLOR_AMBER))
                                .child("PK"),
                        )
                    })
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_size(px(10.5))
                            .text_color(color(COLOR_MUTED))
                            .child(if column.nullable { "NULL" } else { "NOT NULL" }),
                    )
            }))
    }

    fn render_grid_action_strip(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let border = color(COLOR_BORDER);
        let pending_count = self.pending.changes.len();
        let selected = self.selected_row(cx).is_some();

        h_flex()
            .w_full()
            .h(px(32.))
            .flex_shrink_0()
            .items_center()
            .gap_1()
            .px_2()
            .border_t_1()
            .border_color(border)
            .bg(color(COLOR_SURFACE))
            // Data / Structure tabs
            .child(self.grid_mode_button("grid-data", "Data", GridMode::Data, cx))
            .child(self.grid_mode_button("grid-structure", "Structure", GridMode::Structure, cx))
            .child(div().w(px(1.)).h(px(16.)).bg(color(COLOR_BORDER_SOFT)).mx_1())
            // Cell edit field + apply
            .child(div().w(px(220.)).child(Input::new(&self.cell_input).small()))
            .child(
                Button::new("apply")
                    .ghost()
                    .xsmall()
                    .label("Apply")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.apply_cell_edit(window, cx)
                    })),
            )
            .child(div().w(px(1.)).h(px(16.)).bg(color(COLOR_BORDER_SOFT)).mx_1())
            // Row operations
            .child(
                Button::new("add-row")
                    .ghost()
                    .xsmall()
                    .label("+ Row")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.insert_row(window, cx)
                    })),
            )
            .child(
                Button::new("del-row")
                    .ghost()
                    .xsmall()
                    .disabled(!selected)
                    .label("− Row")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.delete_row(window, cx)
                    })),
            )
            .child(div().flex_1())
            // Selection + staged summary
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(color(COLOR_MUTED))
                    .child(if pending_count > 0 {
                        format!("{pending_count} staged")
                    } else {
                        "No staged changes".to_string()
                    }),
            )
    }

    fn grid_mode_button(
        &self,
        id: &'static str,
        label: &'static str,
        mode: GridMode,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let active = self.grid_mode == mode;
        div()
            .id(id)
            .h(px(20.))
            .flex()
            .items_center()
            .px_2()
            .rounded(px(4.))
            .text_size(px(11.5))
            .font_weight(if active { FontWeight::MEDIUM } else { FontWeight::NORMAL })
            .text_color(if active { color(COLOR_TEXT) } else { color(COLOR_MUTED) })
            .when(active, |this| this.bg(color(COLOR_HOVER)))
            .hover(|this| this.text_color(color(COLOR_TEXT)))
            .child(label)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.grid_mode = mode;
                cx.notify();
            }))
    }

    // ---- Console strip ----

    fn render_console(&self, _cx: &mut Context<Self>) -> impl IntoElement {
        let border = color(COLOR_BORDER);

        v_flex()
            .id("console-strip")
            .h(px(140.))
            .flex_shrink_0()
            .min_h_0()
            .bg(color(COLOR_EDITOR))
            .border_t_1()
            .border_color(border)
            .overflow_y_scroll()
            .children(self.console_log.iter().enumerate().map(|(ix, (ok, sql, detail))| {
                h_flex()
                    .id(SharedString::from(format!("log-{ix}")))
                    .min_h(px(22.))
                    .flex_shrink_0()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py(px(2.))
                    .text_size(px(11.))
                    .font_family("Menlo")
                    .border_b_1()
                    .border_color(color(COLOR_BORDER_SOFT))
                    .child(if *ok {
                        Icon::new(IconName::CircleCheck)
                            .xsmall()
                            .text_color(color(COLOR_GREEN))
                    } else {
                        Icon::new(IconName::CircleX).xsmall().text_color(color(COLOR_RED))
                    })
                    .child(
                        div()
                            .min_w_0()
                            .max_w(px(700.))
                            .overflow_hidden()
                            .child(sql.clone()),
                    )
                    .child(div().flex_1())
                    .child(div().text_color(color(COLOR_MUTED)).child(detail.clone()))
            }))
    }

    fn load_history(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some((_, query)) = self.history_entries.get(ix).cloned() else { return };
        self.editor
            .update(cx, |editor, cx| editor.set_value(query, window, cx));
    }

    fn new_table_template(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let template = "CREATE TABLE new_table (\n    id BIGSERIAL PRIMARY KEY,\n    name TEXT NOT NULL,\n    created_at TIMESTAMPTZ DEFAULT now()\n);".to_string();
        self.editor
            .update(cx, |editor, cx| editor.set_value(template, window, cx));
        self.status = "Edit the CREATE TABLE template and run it".into();
        cx.notify();
        let _ = window;
    }

    // ---- Pane 4: row inspector ----

    fn render_inspector(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let border = color(COLOR_BORDER);
        let selected_row = self.selected_row(cx);
        let values = selected_row.and_then(|row| self.rows.get(row));
        let has_row = selected_row.is_some();
        let field_query = self.inspector_search.read(cx).value().trim().to_lowercase();

        v_flex()
            .w(px(300.))
            .flex_shrink_0()
            .min_w_0()
            .h_full()
            .bg(color(COLOR_SURFACE))
            .border_l_1()
            .border_color(border)
            // Details / Assistant tabs
            .child(
                h_flex()
                    .h(px(32.))
                    .px_2()
                    .gap_3()
                    .items_center()
                    .border_b_1()
                    .border_color(color(COLOR_BORDER_SOFT))
                    .child(self.inspector_tab_button("insp-details", "Details", 0, cx))
                    .child(self.inspector_tab_button("insp-assistant", "Assistant", 1, cx)),
            )
            .when(self.inspector_tab == 0, |this| {
                this.child(
                    v_flex()
                        .px_2()
                        .py_2()
                        .child(Input::new(&self.inspector_search).small()),
                )
                .child(self.render_inspector_fields(selected_row, values, &field_query, cx))
            })
            .when(self.inspector_tab == 1, |this| {
                this.child(
                    v_flex()
                        .flex_1()
                        .items_center()
                        .justify_center()
                        .gap_2()
                        .p_4()
                        .child(
                            Icon::new(IconName::Sparkles)
                                .text_color(color(COLOR_BORDER))
                                .size(px(24.)),
                        )
                        .child(
                            div()
                                .text_size(px(11.5))
                                .text_color(color(COLOR_MUTED))
                                .child("Reserved for an opt-in helper"),
                        ),
                )
            })
            // Copy-row toolbar at the bottom of the inspector (full width,
            // so the format buttons never clip).
            .child(
                h_flex()
                    .h(px(34.))
                    .flex_shrink_0()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .border_t_1()
                    .border_color(color(COLOR_BORDER_SOFT))
                    .child(
                        div()
                            .text_size(px(10.5))
                            .text_color(color(COLOR_MUTED))
                            .child("Copy row as"),
                    )
                    .child(div().flex_1())
                    .child(self.copy_button("copy-json", RowCopyFormat::Json, has_row, cx))
                    .child(self.copy_button("copy-yaml", RowCopyFormat::Yaml, has_row, cx))
                    .child(self.copy_button("copy-csv", RowCopyFormat::Csv, has_row, cx)),
            )
    }

    /// Copy-row button in the inspector header: "JSON" / "YAML" / "CSV".
    /// Disabled when no row is selected.
    fn copy_button(
        &self,
        id: &'static str,
        format: RowCopyFormat,
        has_row: bool,
        cx: &mut Context<Self>,
    ) -> Button {
        Button::new(id)
            .ghost()
            .xsmall()
            .label(format.label())
            .disabled(!has_row)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.copy_row_as(format, cx)
            }))
    }

    fn inspector_tab_button(
        &self,
        id: &'static str,
        label: &'static str,
        tab: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let active = self.inspector_tab == tab;
        div()
            .id(id)
            .h_full()
            .flex()
            .items_center()
            .px_1()
            .text_size(px(12.))
            .font_weight(if active { FontWeight::MEDIUM } else { FontWeight::NORMAL })
            .text_color(if active { color(COLOR_TEXT) } else { color(COLOR_MUTED) })
            .when(active, |this| this.border_b_2().border_color(color(COLOR_ACCENT)))
            .hover(|this| this.text_color(color(COLOR_TEXT)))
            .child(label)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.inspector_tab = tab;
                cx.notify();
            }))
    }

    fn render_inspector_fields(
        &self,
        selected_row: Option<usize>,
        values: Option<&Vec<CellValue>>,
        field_query: &str,
        _cx: &mut Context<Self>,
    ) -> impl IntoElement {
        match selected_row {
            None => v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .gap_2()
                .child(
                    Icon::new(IconName::TextCursorInput)
                        .text_color(color(COLOR_BORDER))
                        .size(px(24.)),
                )
                .child(
                    div()
                        .text_size(px(11.5))
                        .text_color(color(COLOR_MUTED))
                        .child("No row selected"),
                )
                .child(
                    div()
                        .text_size(px(10.5))
                        .text_color(color(COLOR_MUTED))
                        .child("Select a row in the grid to inspect it"),
                )
                .into_any_element(),
            Some(_) => v_flex()
                .id("inspector-fields")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .children(self.columns.iter().enumerate().filter(|(ix, column)| {
                    field_query.is_empty() || column.name.to_lowercase().contains(field_query)
                }).map(|(ix, column)| {
                    let value = values
                        .and_then(|row| row.get(ix))
                        .map(|c| c.display())
                        .unwrap_or_default();
                    let is_null = values
                        .and_then(|row| row.get(ix))
                        .is_some_and(|c| matches!(c, CellValue::Null));
                    let truncated: String = value.chars().take(300).collect();
                    v_flex()
                        .px_3()
                        .py(px(6.))
                        .gap_1()
                        .border_b_1()
                        .border_color(color(COLOR_BORDER_SOFT))
                        // Field label + metadata badges
                        .child(
                            h_flex()
                                .gap_1p5()
                                .items_center()
                                .min_w_0()
                                .child(
                                    div()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .text_size(px(11.5))
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(column.name.clone()),
                                )
                                .when(column.is_primary_key, |this| {
                                    this.child(
                                        div()
                                            .px_1p5()
                                            .py(px(0.5))
                                            .rounded(px(3.))
                                            .text_size(px(9.))
                                            .bg(color(0x3A2E14))
                                            .text_color(color(COLOR_AMBER))
                                            .child("PK"),
                                    )
                                })
                                .child(
                                    div()
                                        .text_size(px(9.5))
                                        .text_color(color(COLOR_MUTED))
                                        .child(column.data_type.clone()),
                                )
                                .child(div().flex_1())
                                .when(!column.nullable, |this| {
                                    this.child(
                                        div()
                                            .text_size(px(9.))
                                            .text_color(color(COLOR_MUTED))
                                            .child("NOT NULL"),
                                    )
                                }),
                        )
                        // Field value
                        .child(
                            div()
                                .min_w_0()
                                .font_family("Menlo")
                                .text_size(px(11.5))
                                .when(is_null || truncated.is_empty(), |this| {
                                    this.text_color(color(COLOR_MUTED)).italic().child("NULL")
                                })
                                .when(!is_null && !truncated.is_empty(), |this| {
                                    this.child(truncated)
                                }),
                        )
                }))
                .when(
                    self.columns.iter().all(|c| {
                        field_query.is_empty() || !c.name.to_lowercase().contains(field_query)
                    }) && !field_query.is_empty(),
                    |this| {
                        this.child(
                            div()
                                .p_3()
                                .text_size(px(11.5))
                                .text_color(color(COLOR_MUTED))
                                .child("No fields match the search"),
                        )
                    },
                )
                .into_any_element(),
        }
    }

    // ---- New connection form (collapsed by default) ----

    fn render_connection_form(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let border = color(COLOR_BORDER);

        v_flex()
            .flex_shrink_0()
            .px_3()
            .py_2()
            .gap_1()
            .border_t_1()
            .border_color(border)
            .bg(color(COLOR_SURFACE2))
            .child(
                div()
                    .text_size(px(10.))
                    .text_color(color(COLOR_MUTED))
                    .child("NEW POSTGRESQL CONNECTION"),
            )
            .child(Input::new(&self.form.host).small())
            .child(Input::new(&self.form.port).small())
            .child(Input::new(&self.form.user).small())
            .child(Input::new(&self.form.password).small())
            .child(Input::new(&self.form.database).small())
            .child(
                Button::new("connect-form")
                    .label("Connect")
                    .small()
                    .primary()
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.connect_form(window, cx)
                    })),
            )
    }
}

/// Quote a YAML key; simple identifiers stay bare for readability.
fn yaml_quoted_key(key: &str) -> String {
    let simple = !key.is_empty()
        && key.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-');
    if simple {
        key.to_string()
    } else {
        yaml_double_quoted(key)
    }
}

/// A YAML double-quoted scalar, with escapes for the characters that
/// double-quoted style interprets.
fn yaml_double_quoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

fn whoami_fallback() -> String {
    std::env::var("USER").unwrap_or_else(|_| "postgres".into())
}

const DEMO_SEED: &str = r#"
CREATE TABLE IF NOT EXISTS employees (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    department TEXT,
    salary REAL,
    hired_at TEXT,
    active INTEGER
);
DELETE FROM employees;
INSERT INTO employees (name, department, salary, hired_at, active) VALUES
    ('Ada Lovelace',   'Engineering', 185000.0, '2021-03-15', 1),
    ('Grace Hopper',   'Engineering', 192000.5, '2020-08-01', 1),
    ('Alan Turing',    'Research',    178000.0, '2022-01-10', 0),
    ('Katherine Goble','Research',    165000.0, '2023-06-20', 1),
    ('Edsger Dijkstra','Engineering', 171500.75, '2019-11-30', 1),
    ('Barbara Liskov', 'Research',    188250.0, '2018-05-14', 1);
CREATE TABLE IF NOT EXISTS departments (
    name TEXT PRIMARY KEY,
    floor INTEGER
);
DELETE FROM departments;
INSERT INTO departments (name, floor) VALUES
    ('Engineering', 3),
    ('Research', 5);
"#;
