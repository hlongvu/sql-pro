//! The main window: sidebar (connections + tables + history), SQL editor,
//! results grid with editable cells, and status bar. Mirrors TablePro's
//! MainSplitViewController and its edit → change-tracking → save pipeline.

use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Editor, EditorState, Input, InputState};
use gpui_kit::component::table::TableState;
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Disableable, Sizable, Size};
use gpui_kit::prelude::*;
use gpui_kit::*;

use sqlpro_core::change::{PendingChanges, RowID};
use sqlpro_core::connection::{validate, ConnectionConfig};
use sqlpro_core::database_type::DatabaseType;
use sqlpro_core::history::{QueryHistoryEntry, QueryHistoryFilter, QueryHistoryStorage};
use sqlpro_core::result::QueryResult;
use sqlpro_core::schema::{ColumnInfo, TableInfo};
use sqlpro_core::value::CellValue;
use sqlpro_core::write::{
    engine_reports_real_counts, DataWriteExecutor, DataWritePlan, ParameterizedExecutor,
    TransactionOwner,
};
use sqlpro_core::sqlgen::{SqlStatementGenerator, DEFAULT_MARKER};

use crate::grid::{ResultsDelegate, RowVisual};
use crate::store;

actions!(workspace, [RunQuery]);

const DEFAULT_ROW_LIMIT: usize = 500;
const HISTORY_PANEL_SIZE: usize = 10;

pub struct Workspace {
    connections: Vec<ConnectionConfig>,
    active: Option<Arc<ConnectionConfig>>,
    driver: Option<Arc<dyn ParameterizedExecutor>>,
    tables: Vec<TableInfo>,
    status: String,
    running: bool,

    // Current result set (the grid's model).
    table_name: String,
    pk_columns: Vec<String>,
    columns: Vec<ColumnInfo>,
    /// The row as read from the server, kept for old-value tracking and
    /// keyless matching — TablePro's originalRow.
    original_rows: Vec<Vec<CellValue>>,
    /// What the grid displays: originals with staged edits applied, plus
    /// inserted rows.
    rows: Vec<Vec<CellValue>>,
    row_ids: Vec<RowID>,

    pending: PendingChanges,
    history: Option<QueryHistoryStorage>,
    history_entries: Vec<(String, String)>, // (id, query)

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
        let editor = cx.new(|cx| EditorState::new(window, cx).placeholder("Type SQL… (⌘⏎ to run)"));
        let table = cx.new(|cx| {
            TableState::new(ResultsDelegate::empty(), window, cx)
                .col_resizable(true)
                .row_selectable(true)
                .cell_selectable(true)
        });
        let cell_input = cx.new(|cx| InputState::new(window, cx).placeholder("Selected cell value — Apply to stage"));
        let form = ConnectionForm {
            host: cx.new(|cx| InputState::new(window, cx).placeholder("host (localhost)")),
            port: cx.new(|cx| InputState::new(window, cx).placeholder("port")),
            user: cx.new(|cx| InputState::new(window, cx).placeholder("user")),
            password: cx.new(|cx| InputState::new(window, cx).placeholder("password")),
            database: cx.new(|cx| InputState::new(window, cx).placeholder("database")),
        };

        let history = QueryHistoryStorage::open(&sqlpro_core::history::default_history_path()).ok();
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
            status: "Ready".to_string(),
            running: false,
            table_name: String::new(),
            pk_columns: Vec::new(),
            columns: Vec::new(),
            original_rows: Vec::new(),
            rows: Vec::new(),
            row_ids: Vec::new(),
            pending: PendingChanges::new(),
            history,
            history_entries,
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
        let run_key = if cfg!(target_os = "macos") {
            "cmd-enter"
        } else {
            "ctrl-enter"
        };
        cx.bind_keys([KeyBinding::new(run_key, RunQuery, None)]);
    }

    // ---- Connections ---------------------------------------------------

    fn connect_demo(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut config =
            ConnectionConfig::new("Demo (SQLite :memory:)", DatabaseType::new(DatabaseType::SQLITE));
        config.path = Some(":memory:".into());
        let seed: Option<std::sync::Arc<str>> = Some(std::sync::Arc::from(DEMO_SEED));
        self.connect(Arc::new(config), seed, window, cx);
    }

    fn connect_saved(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(config) = self.connections.get(ix).cloned() else {
            return;
        };
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
            cx.notify();
            return;
        }

        if !self.connections.iter().any(|c| c.id == config.id) {
            self.connections.push(config.clone());
            store::save_connections(&self.connections);
        }
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
        cx.notify();

        cx.spawn_in(window, async move |this, cx| {
            let connect_config = Arc::clone(&config);
            eprintln!("connecting: {}…", config.name);
            let connect = sqlpro_drivers::runtime::on_thread(move || {
                sqlpro_drivers::create_driver(&connect_config)
            })
            .await;
            eprintln!("create_driver done: {:?}", connect.is_ok());
            let outcome: std::result::Result<Arc<dyn ParameterizedExecutor>, String> = match connect {
                Ok(driver) => Ok(Arc::from(driver)),
                Err(e) => Err(e.to_string()),
            };

            // Seed the demo database right after connecting.
            if let (Ok(driver), Some(seed)) = (&outcome, &seed) {
                if let Err(e) = driver.execute(seed).await {
                    eprintln!("demo seed failed: {e}");
                }
            }

            eprintln!("fetching tables…");
            let tables = match &outcome {
                Ok(driver) => driver.list_tables().await.unwrap_or_default(),
                Err(_) => Vec::new(),
            };
            eprintln!("tables fetched: {}", tables.len());

            _ = this.update(cx, |ws, cx| {
                ws.running = false;
                match outcome {
                    Ok(driver) => {
                        eprintln!(
                            "connected: {} ({} tables)",
                            config.name,
                            tables.len()
                        );
                        ws.status = format!("Connected: {}", driver.label());
                        ws.driver = Some(driver);
                        ws.active = Some(config);
                        ws.tables = tables;
                    }
                    Err(e) => {
                        eprintln!("connection failed: {e}");
                        ws.status = format!("Connection failed: {e}");
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
            let elapsed = started.elapsed();

            _ = this.update(cx, |ws, cx| {
                ws.running = false;
                match result {
                    Ok(result) => {
                        ws.status = format!(
                            "{} — {} rows, {:.0} ms",
                            if result.is_query { "Query" } else { "Statement" },
                            if result.is_query { result.rows.len() } else { result.rows_affected as usize },
                            elapsed.as_secs_f64() * 1000.0,
                        );
                        ws.apply_result(result, cx);
                        ws.record_history(&sql, elapsed, true, None, cx);
                    }
                    Err(e) => {
                        ws.status = format!("Error: {e}");
                        ws.record_history(&sql, elapsed, false, Some(&e.to_string()), cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn record_history(
        &mut self,
        sql: &str,
        elapsed: std::time::Duration,
        success: bool,
        error: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        let Some(connection) = self.active.clone() else { return };
        let mut entry = QueryHistoryEntry::new(sql, connection.id.to_string(), &connection.database_type);
        entry.execution_time = elapsed.as_secs_f64();
        entry.was_successful = success;
        entry.error_message = error.map(|e| e.to_string());
        entry.row_count = -1;
        if let Some(history) = &self.history {
            let _ = history.record(&entry);
        }
        self.history_entries.insert(0, (entry.id.clone(), entry.query.clone()));
        self.history_entries.truncate(HISTORY_PANEL_SIZE);
        let _ = cx;
    }

    fn apply_result(&mut self, result: QueryResult, cx: &mut Context<Self>) {
        let max_columns = result.columns.len();
        let _ = max_columns;
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
        let Some(table) = self.tables.get(ix) else {
            return;
        };
        let sql = format!(
            "SELECT * FROM \"{}\" LIMIT {DEFAULT_ROW_LIMIT}",
            table.name.replace('"', "\"\"")
        );
        let sql_value = sql.clone();
        self.editor.update(cx, |editor, cx| editor.set_value(sql_value, window, cx));
        self.table_name = table.name.clone();

        // Load the table's primary key so the save path can use keyed
        // matching, then run the query.
        let table_for_columns = table.name.clone();
        let driver = self.driver.clone();
        cx.spawn_in(window, async move |this, cx| {
            let pk = if let Some(driver) = driver {
                let table = table_for_columns.clone();
                sqlpro_drivers::runtime::on_tokio(async move { driver.list_columns(&table).await })
                    .await
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|c| c.is_primary_key)
                    .map(|c| c.name)
                    .collect::<Vec<String>>()
            } else {
                Vec::new()
            };
            _ = this.update(cx, |ws, cx| {
                ws.pk_columns = pk;
                cx.notify();
            });
        })
        .detach();

        self.run_query(&RunQuery, window, cx);
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

        // Keep the pre-edit original for old-value tracking.
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
            self.status = format!("Staged: {} cell(s) edited, {} row(s) deleted, {} row(s) inserted",
                self.pending.modified_cells.values().map(|s| s.len()).sum::<usize>(),
                self.pending.deleted_row_ids.len(),
                self.pending.inserted_row_ids.len());
        }
        let _ = window;
        self.refresh_grid(cx);
        cx.notify();
    }

    fn insert_row(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.columns.is_empty() {
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
        // Display blanks; the DEFAULT markers stay in inserted_row_data so
        // the server fills them in.
        self.rows.push(vec![CellValue::Null; self.columns.len()]);
        self.row_ids.push(row_id);
        self.refresh_grid(cx);
        self.status = "Staged: new row".into();
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
            // An inserted row never reached the server: just unstage it.
            self.pending.undo_row_insertion(row_id);
            self.rows.remove(row_ix);
            self.row_ids.remove(row_ix);
        } else {
            let original_row = self.original_rows.get(row_ix).cloned().unwrap_or_default();
            self.pending
                .record_row_deletion(row_id, original_row, Default::default());
        }
        self.refresh_grid(cx);
        self.status = "Staged: row deletion".into();
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
                let plan = DataWritePlan::from_attributed(attributed, has_pk, TransactionOwner::App);
                let executor = DataWriteExecutor { driver: driver.as_ref(), real_counts };
                executor.run(&plan).await
            })
            .await;

            _ = this.update_in(cx, |ws, window, cx| {
                match outcome {
                    Ok(result) => {
                        let affected: usize =
                            result.executed.iter().map(|s| s.rows_affected as usize).sum();
                        ws.status = format!(
                            "Saved: {} statement(s), {affected} row(s) affected",
                            result.executed.len()
                        );
                        ws.pending.clear();
                        ws.reload_current_table(window, cx);
                    }
                    Err(e) => {
                        ws.status = format!("Save failed: {e}");
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn reload_current_table(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.table_name.is_empty() {
            // Without a named table (arbitrary SQL result), just reset the
            // grid to what is in memory.
            self.rows = self.original_rows.clone();
            self.row_ids = (0..self.rows.len() as i64).map(RowID::Existing).collect();
            self.refresh_grid(cx);
            return;
        }
        let sql = format!(
            "SELECT * FROM \"{}\" LIMIT {DEFAULT_ROW_LIMIT}",
            self.table_name.replace('"', "\"\"")
        );
        self.editor.update(cx, |editor, cx| editor.set_value(sql, window, cx));
        self.run_query(&RunQuery, window, cx);
    }

    fn revert_changes(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.pending.clear();
        // Restore the display to the server truth.
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
                self.status = format!("Exported {} rows to {}", self.rows.len(), path.display());
            }
            Err(e) => {
                self.status = format!("Export failed: {e}");
            }
        }
        cx.notify();
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

    /// Recompute the grid delegate from the current rows and pending state.
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
}

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let pending_count = self.pending.changes.len();

        v_flex()
            .id("workspace")
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .on_action(cx.listener(Self::run_query))
            .child(
                // Toolbar
                h_flex()
                    .w_full()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        Button::new("run")
                            .label(if self.running { "Running…" } else { "Run ⌘⏎" })
                            .primary()
                            .disabled(self.running || self.driver.is_none())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.run_query(&RunQuery, window, cx)
                            })),
                    )
                    .child(
                        Button::new("demo")
                            .label("Demo DB")
                            .ghost()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.connect_demo(window, cx)
                            })),
                    )
                    .child(
                        Button::new("export")
                            .label("Export CSV")
                            .ghost()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.export_csv(window, cx)
                            })),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_color(theme.muted_foreground)
                            .child(self.active.as_ref().map(|c| c.name.clone()).unwrap_or_else(|| "No connection".into())),
                    ),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_sidebar(cx))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_h_0()
                            .border_l_1()
                            .border_color(theme.border)
                            .child(
                                div()
                                    .border_b_1()
                                    .border_color(theme.border)
                                    .p_2()
                                    .child(Editor::new(&self.editor).h(px(120.))),
                            )
                            // Edit toolbar
                            .child(
                                h_flex()
                                    .gap_2()
                                    .px_2()
                                    .py_1()
                                    .border_b_1()
                                    .border_color(theme.border)
                                    .child(
                                        div()
                                            .w(px(280.))
                                            .child(Input::new(&self.cell_input)),
                                    )
                                    .child(
                                        Button::new("apply")
                                            .label("Apply cell")
                                            .with_size(Size::Small)
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.apply_cell_edit(window, cx)
                                            })),
                                    )
                                    .child(
                                        Button::new("insert")
                                            .label("+ Row")
                                            .with_size(Size::Small)
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.insert_row(window, cx)
                                            })),
                                    )
                                    .child(
                                        Button::new("delete")
                                            .label("− Row")
                                            .with_size(Size::Small)
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.delete_row(window, cx)
                                            })),
                                    )
                                    .child(div().flex_1())
                                    .child(
                                        div()
                                            .text_color(theme.muted_foreground)
                                            .text_sm()
                                            .child(format!("{pending_count} pending")),
                                    )
                                    .child(
                                        Button::new("revert")
                                            .label("Revert")
                                            .with_size(Size::Small)
                                            .ghost()
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.revert_changes(window, cx)
                                            })),
                                    )
                                    .child(
                                        Button::new("save")
                                            .label("Save")
                                            .with_size(Size::Small)
                                            .primary()
                                            .disabled(pending_count == 0)
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.save_changes(window, cx)
                                            })),
                                    ),
                            )
                            .child(div().flex_1().min_h_0().p_1().child(self.table.clone())),
                    ),
            )
            .child(
                // Status bar
                h_flex()
                    .w_full()
                    .px_3()
                    .py_1()
                    .border_t_1()
                    .border_color(theme.border)
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(self.status.clone()),
            )
    }
}

impl Workspace {
    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();

        v_flex()
            .id("sidebar")
            .w(px(250.))
            .min_w_0()
            .h_full()
            .bg(theme.sidebar)
            .overflow_y_scroll()
            .child(
                v_flex()
                    .px_3()
                    .py_2()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("CONNECTIONS"),
                    )
                    .children(self.connections.iter().enumerate().map(|(ix, conn)| {
                        let selected = self
                            .active
                            .as_ref()
                            .map(|a| a.id == conn.id)
                            .unwrap_or(false);
                        div()
                            .id(SharedString::from(format!("conn-{ix}")))
                            .px_2()
                            .py_1()
                            .rounded_sm()
                            .text_sm()
                            .when(selected, |this| {
                                this.bg(theme.accent).text_color(gpui::white())
                            })
                            .hover(|this| this.bg(theme.secondary))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.connect_saved(ix, window, cx)
                            }))
                            .child(conn.name.clone())
                    })),
            )
            .child(
                v_flex()
                    .px_3()
                    .py_2()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("TABLES"),
                    )
                    .children(self.tables.iter().enumerate().map(|(ix, table)| {
                        let kind = if table.is_view { "view" } else { "table" };
                        div()
                            .id(SharedString::from(format!("table-{ix}")))
                            .px_2()
                            .py_1()
                            .rounded_sm()
                            .text_sm()
                            .hover(|this| this.bg(theme.secondary))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_table(ix, window, cx)
                            }))
                            .child(format!("{} ({})", table.name, kind))
                    }))
                    .when(self.tables.is_empty(), |this| {
                        this.child(
                            div()
                                .px_2()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child("No tables"),
                        )
                    }),
            )
            .child(
                v_flex()
                    .px_3()
                    .py_2()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("QUERY HISTORY"),
                    )
                    .children(self.history_entries.iter().enumerate().map(|(ix, (_, query))| {
                        let one_line = query.lines().next().unwrap_or("").to_string();
                        let truncated: String = one_line.chars().take(40).collect();
                        div()
                            .id(SharedString::from(format!("history-{ix}")))
                            .px_2()
                            .py_1()
                            .rounded_sm()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .hover(|this| this.bg(theme.secondary))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.load_history(ix, window, cx)
                            }))
                            .child(truncated)
                    })),
            )
            .child(self.render_connection_form(cx))
    }

    fn load_history(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some((_, query)) = self.history_entries.get(ix).cloned() else { return };
        self.editor.update(cx, |editor, cx| editor.set_value(query, window, cx));
    }

    fn render_connection_form(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();

        v_flex()
            .px_3()
            .py_2()
            .gap_1()
            .border_t_1()
            .border_color(theme.border)
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("NEW POSTGRESQL CONNECTION"),
            )
            .child(Input::new(&self.form.host))
            .child(Input::new(&self.form.port))
            .child(Input::new(&self.form.user))
            .child(Input::new(&self.form.password))
            .child(Input::new(&self.form.database))
            .child(
                Button::new("connect-form")
                    .label("Connect")
                    .with_size(Size::Small)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.connect_form(window, cx)
                    })),
            )
    }
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
