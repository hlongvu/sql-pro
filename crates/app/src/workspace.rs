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
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Editor, EditorState, Input, InputEvent, InputState};
use gpui_kit::component::menu::ContextMenuExt;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::table::{TableEvent, TableState};
use gpui_kit::component::WindowExt;
use gpui_kit::component::{h_flex, v_flex, Disableable, Icon, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::*;

use sqlpro_core::change::{PendingChanges, RowID};
use sqlpro_core::connection::{
    parse_connection_url, validate, ConnectionConfig, SslMode, SshAuth, SshTunnelConfig,
};
use sqlpro_core::database_type::DatabaseType;
use sqlpro_core::history::{QueryHistoryEntry, QueryHistoryFilter, QueryHistoryStorage};
use sqlpro_core::result::QueryResult;
use sqlpro_core::schema::{ColumnInfo, TableInfo};
use sqlpro_core::sqlgen::{SqlStatementGenerator, DEFAULT_MARKER};
use sqlpro_core::value::CellValue;
use sqlpro_core::write::{
    engine_reports_real_counts, DataWriteExecutor, DataWritePlan, DataWriteResult,
    ParameterizedExecutor, TransactionOwner,
};

use crate::grid::{ResultsDelegate, RowVisual};
use crate::store;

actions!(
    workspace,
    [
        RunQuery, NewQuery, SaveChanges,
        EditCell, CopyCell, CopyRowJson, CopyRowYaml, CopyRowCsv, InsertRow, DeleteRow,
        CancelCellEdit,
    ]
);

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
    objects_tab: usize,   // 0 = Items, 1 = History
    inspector_tab: usize, // 0 = Details, 1 = Assistant
    grid_mode: GridMode,
    safe_mode: SafeMode,
    dark_mode: bool,
    /// Two-step guard: closing with staged changes needs a second press.
    close_confirmed: bool,
    /// Database switcher on the same server (objects-pane footer popover).
    db_switch_open: bool,
    db_switch_loading: bool,
    discovered_dbs: Vec<String>,
    conn_search: Entity<InputState>,
    objects_search: Entity<InputState>,
    inspector_search: Entity<InputState>,

    editor: Entity<EditorState>,
    table: Entity<TableState<ResultsDelegate>>,
    /// Inline cell editor: rendered directly inside the cell being edited.
    cell_editor: Entity<InputState>,
    editing_cell: Option<(usize, usize)>,
    /// New-connection dialog (engine picker → details form / URL import).
    pub(crate) dialog_open: bool,
    pub(crate) dialog: ConnDialog,
    /// A "Test" from the dialog is in flight (disables the button).
    conn_testing: bool,
}

/// Which screen of the new-connection dialog is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConnDialogStep {
    /// The engine grid (screenshot 1).
    PickType,
    /// The connection details form (screenshot 2).
    Details,
    /// Paste a connection URL (the "Import from URL" button).
    ImportUrl,
}

/// One row of the engine catalog shown in the picker grid. Every entry maps
/// to a `DatabaseType` id; only some have drivers in this binary.
struct EngineCatalogEntry {
    id: &'static str,
    label: &'static str,
    letters: &'static str,
    color: u32,
}

const ENGINE_CATALOG: &[EngineCatalogEntry] = &[
    EngineCatalogEntry { id: DatabaseType::POSTGRESQL, label: "PostgreSQL", letters: "Pg", color: 0x3E8ECC },
    EngineCatalogEntry { id: DatabaseType::MYSQL, label: "MySQL", letters: "Ms", color: 0xE8912D },
    EngineCatalogEntry { id: "mariadb", label: "MariaDB", letters: "Mr", color: 0x2FA39A },
    EngineCatalogEntry { id: "mssql", label: "Microsoft SQL Server", letters: "Ss", color: 0x9095A0 },
    EngineCatalogEntry { id: "cassandra", label: "Cassandra", letters: "Cs", color: 0x3C3F45 },
    EngineCatalogEntry { id: "mongodb", label: "Mongo (Beta)", letters: "Mg", color: 0x4FA65C },
    EngineCatalogEntry { id: "snowflake", label: "Snowflake", letters: "Nf", color: 0x35B5E5 },
    EngineCatalogEntry { id: "redis", label: "Redis", letters: "Re", color: 0xCB4232 },
    EngineCatalogEntry { id: DatabaseType::SQLITE, label: "SQLite", letters: "Sl", color: 0x8E6EC8 },
    EngineCatalogEntry { id: "oracle", label: "Oracle", letters: "Oc", color: 0xE8493A },
    EngineCatalogEntry { id: "cockroachdb", label: "Cockroach", letters: "Cr", color: 0x53A653 },
    EngineCatalogEntry { id: "vertica", label: "Vertica", letters: "Ve", color: 0x2C5F8A },
];

/// Status color swatches offered on the details form.
const STATUS_COLORS: [u32; 6] = [0x6EC878, 0xA06CE8, 0xE8C858, 0x4FC4C4, 0xF08CB8, 0xE85A5A];

fn catalog_entry(db: &DatabaseType) -> Option<&'static EngineCatalogEntry> {
    ENGINE_CATALOG.iter().find(|e| e.id == db.id())
}

fn next_ssl_mode(mode: SslMode) -> SslMode {
    match mode {
        SslMode::Disable => SslMode::Prefer,
        SslMode::Prefer => SslMode::Require,
        SslMode::Require => SslMode::VerifyFull,
        SslMode::VerifyFull => SslMode::Disable,
    }
}

fn ssl_mode_label(mode: SslMode) -> &'static str {
    match mode {
        SslMode::Disable => "DISABLED",
        SslMode::Prefer => "PREFERRED",
        SslMode::Require => "REQUIRED",
        SslMode::VerifyFull => "VERIFY FULL",
    }
}

/// All inputs of the new-connection dialog. Entities live for the app's
/// lifetime; `open` resets them so stale values never leak between adds.
/// Fields are `pub(crate)` so headless tests can drive the dialog.
pub(crate) struct ConnDialog {
    pub(crate) step: ConnDialogStep,
    pub(crate) db_type: DatabaseType,
    /// Index into ENGINE_CATALOG, None until the user picks one (or a URL
    /// import sets an engine outside the catalog, e.g. redshift).
    pub(crate) engine_ix: Option<usize>,
    pub(crate) color_ix: Option<usize>,
    pub(crate) store_password: bool,
    pub(crate) over_ssh: bool,
    pub(crate) ssl: SslMode,
    pub(crate) name: Entity<InputState>,
    pub(crate) tag: Entity<InputState>,
    pub(crate) host: Entity<InputState>,
    pub(crate) port: Entity<InputState>,
    pub(crate) user: Entity<InputState>,
    pub(crate) password: Entity<InputState>,
    pub(crate) database: Entity<InputState>,
    pub(crate) url: Entity<InputState>,
    pub(crate) ssh_host: Entity<InputState>,
    pub(crate) ssh_port: Entity<InputState>,
    pub(crate) ssh_user: Entity<InputState>,
    pub(crate) ssh_password: Entity<InputState>,
}

impl ConnDialog {
    fn new(window: &mut Window, cx: &mut Context<Workspace>) -> Self {
        Self {
            step: ConnDialogStep::PickType,
            db_type: DatabaseType::new(DatabaseType::POSTGRESQL),
            engine_ix: None,
            color_ix: None,
            store_password: true,
            over_ssh: false,
            ssl: SslMode::Prefer,
            name: cx.new(|cx| InputState::new(window, cx).placeholder("My connection")),
            tag: cx.new(|cx| InputState::new(window, cx).placeholder("local")),
            host: cx.new(|cx| InputState::new(window, cx).placeholder("127.0.0.1")),
            port: cx.new(|cx| InputState::new(window, cx).placeholder("5432")),
            user: cx.new(|cx| InputState::new(window, cx).placeholder("user")),
            password: cx.new(|cx| InputState::new(window, cx).placeholder("password").masked(true)),
            database: cx.new(|cx| InputState::new(window, cx).placeholder("database")),
            url: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("postgresql://user:password@host:5432/database")
            }),
            ssh_host: cx.new(|cx| InputState::new(window, cx).placeholder("SSH host")),
            ssh_port: cx.new(|cx| InputState::new(window, cx).placeholder("22")),
            ssh_user: cx.new(|cx| InputState::new(window, cx).placeholder("SSH user")),
            ssh_password: cx.new(|cx| {
                InputState::new(window, cx).placeholder("SSH password").masked(true)
            }),
        }
    }

    /// Clear everything when the dialog re-opens for a new connection.
    fn reset(&mut self, window: &mut Window, cx: &mut Context<Workspace>) {
        self.step = ConnDialogStep::PickType;
        self.db_type = DatabaseType::new(DatabaseType::POSTGRESQL);
        self.engine_ix = None;
        self.color_ix = None;
        self.store_password = true;
        self.over_ssh = false;
        self.ssl = SslMode::Prefer;
        for input in [
            &self.name,
            &self.tag,
            &self.host,
            &self.port,
            &self.user,
            &self.password,
            &self.database,
            &self.url,
            &self.ssh_host,
            &self.ssh_port,
            &self.ssh_user,
            &self.ssh_password,
        ] {
            input.update(cx, |state, cx| state.set_value("", window, cx));
        }
    }

    fn read(cx: &App, input: &Entity<InputState>) -> String {
        input.read(cx).value().trim().to_string()
    }
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
        let cell_editor = cx.new(|cx| InputState::new(window, cx).placeholder(""));
        let conn_search =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search connections"));
        let objects_search = cx.new(|cx| InputState::new(window, cx).placeholder("Search objects"));
        let inspector_search = cx.new(|cx| InputState::new(window, cx).placeholder("Search fields"));
        let dialog = ConnDialog::new(window, cx);

        let history =
            QueryHistoryStorage::open(&sqlpro_core::history::default_history_path()).ok();

        // Right-click on the grid moves the selection to the clicked
        // row/cell, so context-menu actions operate on the right-clicked
        // target even without a prior left-click. Double-click starts
        // inline editing directly on the cell.
        cx.subscribe_in(&table, window, |this, _, event: &TableEvent, window, cx| {
            match event {
                TableEvent::RightClickedCell(row, col) => {
                    this.table.update(cx, |t, cx| t.set_selected_cell(*row, *col, cx));
                }
                TableEvent::RightClickedRow(Some(row)) => {
                    this.table.update(cx, |t, cx| t.set_selected_row(*row, cx));
                }
                TableEvent::DoubleClickedCell(row, col) => {
                    let cell = (*row, *col);
                    this.begin_cell_edit(Some(cell), window, cx);
                }
                _ => {}
            }
        })
        .detach();

        // ⏎ in the inline cell editor stages the value ("confirm but don't
        // apply yet"); ⌘S is what writes staged changes to the database.
        // Clicking away (blur) stages the in-progress value too.
        cx.subscribe_in(&cell_editor, window, |this, _, event: &InputEvent, window, cx| {
            match event {
                InputEvent::PressEnter { .. } => this.commit_cell_edit(window, cx),
                InputEvent::Blur => this.commit_cell_edit(window, cx),
                _ => {}
            }
        })
        .detach();

        // ⏎ in the URL field of the connection dialog applies the import.
        cx.subscribe_in(&dialog.url, window, |this, _, event: &InputEvent, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. })
                && this.dialog_open
                && this.dialog.step == ConnDialogStep::ImportUrl
            {
                this.dialog_apply_url(window, cx);
            }
        })
        .detach();

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
            objects_tab: 0,
            inspector_tab: 0,
            grid_mode: GridMode::Data,
            safe_mode: SafeMode::Warn,
            dark_mode: true,
            close_confirmed: false,
            db_switch_open: false,
            db_switch_loading: false,
            discovered_dbs: Vec::new(),
            conn_search,
            objects_search,
            inspector_search,
            editor,
            table,
            cell_editor,
            editing_cell: None,
            dialog_open: false,
            dialog,
            conn_testing: false,
        };

        // Keyboard shortcuts (⌘S save, ⌘⏎ run, ⌘E/⌘T new query, Escape
        // cancel) are registered directly as window key listeners in
        // `render` rather than through the keymap, so they fire no matter
        // which element holds focus.

        // The macOS close button closes the *connection*, not the app:
        // while a database is connected the window stays open and shows the
        // welcome screen again. Staged changes require a second press.
        {
            let entity = cx.entity();
            window.on_window_should_close(cx, move |_window, cx| {
                let allow_close = std::cell::Cell::new(true);
                let _ = entity.update(cx, |ws, cx| {
                    if ws.driver.is_none() {
                        // Welcome screen: the red button quits the app.
                        return;
                    }
                    if ws.running {
                        ws.status = "Busy — wait for the current operation to finish".into();
                        allow_close.set(false);
                        return;
                    }
                    if !ws.pending.is_empty() && !ws.close_confirmed {
                        ws.close_confirmed = true;
                        ws.status = format!(
                            "{} staged change(s) will be discarded — press close again to confirm",
                            ws.pending.changes.len()
                        );
                        allow_close.set(false);
                        return;
                    }
                    allow_close.set(false);
                    ws.disconnect(cx);
                });
                allow_close.into_inner()
            });
        }

        // Optional auto-connect for a saved connection, e.g.
        // SQLPRO_CONNECT="dental_hub (local)". Without it, the app opens on
        // the welcome/start screen (shown while no driver is connected).
        if let Ok(name) = std::env::var("SQLPRO_CONNECT") {
            match ws.connections.iter().position(|c| c.name == name) {
                Some(ix) => ws.connect_saved(ix, window, cx),
                None => {
                    ws.status = format!("No saved connection named \"{name}\"");
                    ws.connect_demo(window, cx);
                }
            }
        }

        ws
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
        self.colored_dot(state, None)
    }

    /// The state dot, optionally tinted with the connection's own status
    /// color (TablePlus colors list rows with the chosen swatch).
    fn colored_dot(&self, state: ConnState, custom: Option<u32>) -> Div {
        let c = custom.unwrap_or(match state {
            ConnState::Connected => COLOR_GREEN,
            ConnState::Connecting => COLOR_AMBER,
            ConnState::Offline => COLOR_MUTED,
        });
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
            self.toast(
                Notification::info(format!("Already connected: {}", config.name)),
                window,
                cx,
            );
            cx.notify();
            return;
        }
        self.connect(Arc::new(config), None, window, cx);
    }

    // ---- New-connection dialog -----------------------------------------

    pub(crate) fn open_conn_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dialog.reset(window, cx);
        self.dialog_open = true;
        cx.notify();
    }

    fn close_conn_dialog(&mut self, cx: &mut Context<Self>) {
        self.dialog_open = false;
        cx.notify();
    }

    /// Test hook: borrow one of the dialog's inputs by field.
    pub(crate) fn dialog_input(
        &self,
        pick: impl Fn(&ConnDialog) -> &Entity<InputState>,
    ) -> &Entity<InputState> {
        pick(&self.dialog)
    }

    pub(crate) fn dialog_pick_engine(&mut self, ix: usize, cx: &mut Context<Self>) {
        if let Some(entry) = ENGINE_CATALOG.get(ix) {
            self.dialog.engine_ix = Some(ix);
            self.dialog.db_type = DatabaseType::new(entry.id);
        }
        cx.notify();
    }

    /// The "Create" button on the picker: advance to the details form.
    pub(crate) fn dialog_advance(&mut self, cx: &mut Context<Self>) {
        if self.dialog.engine_ix.is_some() {
            self.dialog.step = ConnDialogStep::Details;
            cx.notify();
        }
    }

    pub(crate) fn dialog_apply_url(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let url = ConnDialog::read(cx, &self.dialog.url);
        let parsed = match parse_connection_url(&url) {
            Ok(p) => p,
            Err(e) => {
                self.toast(Notification::error(e).title("Import from URL"), window, cx);
                return;
            }
        };
        let mut set = |input: &Entity<InputState>, value: &str, cx: &mut App| {
            input.update(cx, |state, cx| state.set_value(value, window, cx));
        };
        set(&self.dialog.host, parsed.host.as_deref().unwrap_or(""), cx);
        set(
            &self.dialog.port,
            &parsed.port.map(|p| p.to_string()).unwrap_or_default(),
            cx,
        );
        set(&self.dialog.user, parsed.username.as_deref().unwrap_or(""), cx);
        set(&self.dialog.password, parsed.password.as_deref().unwrap_or(""), cx);
        set(&self.dialog.database, parsed.database.as_deref().unwrap_or(""), cx);
        if let Some(ssl) = parsed.ssl_mode {
            self.dialog.ssl = ssl;
        }
        self.dialog.db_type = parsed.database_type.clone();
        self.dialog.engine_ix = catalog_entry(&parsed.database_type).map(|e| {
            ENGINE_CATALOG
                .iter()
                .position(|entry| entry.id == e.id)
                .unwrap_or(0)
        });
        self.dialog.step = ConnDialogStep::Details;
        let engine = catalog_entry(&parsed.database_type)
            .map(|e| e.label.to_string())
            .unwrap_or_else(|| parsed.database_type.to_string());
        self.log_console(true, "import url", &format!("{engine} {url}"));
        self.toast(
            Notification::success(format!("Parsed {engine} connection URL")),
            window,
            cx,
        );
        cx.notify();
    }

    /// Read the dialog inputs into a `ConnectionConfig` (nothing persisted).
    pub(crate) fn dialog_build_config(
        &self,
        cx: &App,
    ) -> std::result::Result<ConnectionConfig, String> {
        let is_sqlite = self.dialog.db_type.driver_id() == "sqlite";
        let host = ConnDialog::read(cx, &self.dialog.host);
        let database = ConnDialog::read(cx, &self.dialog.database);
        let port: Option<u16> = ConnDialog::read(cx, &self.dialog.port).parse().ok();
        let user = ConnDialog::read(cx, &self.dialog.user);
        let password = ConnDialog::read(cx, &self.dialog.password);
        let name_input = ConnDialog::read(cx, &self.dialog.name);

        let mut config = ConnectionConfig::new(
            "pending",
            self.dialog.db_type.clone(),
        );
        if is_sqlite {
            config.path = (!database.is_empty()).then_some(database.clone());
            if name_input.is_empty() {
                let file = database.rsplit('/').next().unwrap_or("SQLite");
                config.name = format!("SQLite · {file}");
            }
        } else {
            if host.is_empty() {
                return Err("Host is required".into());
            }
            config.host = Some(host.clone());
            config.port = port;
            config.username = (!user.is_empty()).then_some(user.clone());
            config.password = (!password.is_empty()).then_some(password);
            config.database = (!database.is_empty()).then_some(database.clone());
            config.ssl_mode = self.dialog.ssl;
        }
        if name_input.is_empty() && !is_sqlite {
            config.name = format!(
                "{}:{}{}",
                host,
                port.or_else(|| config.database_type.default_port()).unwrap_or(0),
                if database.is_empty() {
                    String::new()
                } else {
                    format!("/{database}")
                }
            );
        } else if !name_input.is_empty() {
            config.name = name_input;
        }
        config.color = self.dialog.color_ix.map(|ix| STATUS_COLORS[ix]);
        let tag = ConnDialog::read(cx, &self.dialog.tag);
        config.tag = (!tag.is_empty()).then_some(tag);

        if self.dialog.over_ssh {
            let ssh_host = ConnDialog::read(cx, &self.dialog.ssh_host);
            if ssh_host.is_empty() {
                return Err("SSH host is required when Over SSH is on".into());
            }
            config.ssh_tunnel = Some(SshTunnelConfig {
                host: ssh_host,
                port: ConnDialog::read(cx, &self.dialog.ssh_port)
                    .parse()
                    .unwrap_or(22),
                username: {
                    let u = ConnDialog::read(cx, &self.dialog.ssh_user);
                    if u.is_empty() { whoami_fallback() } else { u }
                },
                auth: SshAuth::Password {
                    password: ConnDialog::read(cx, &self.dialog.ssh_password),
                },
            });
        }

        validate(&config).map_err(|e| e.to_string())?;
        Ok(config)
    }

    /// Persist the dialog's connection without opening it.
    fn dialog_save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut config = match self.dialog_build_config(cx) {
            Ok(c) => c,
            Err(e) => {
                self.toast(Notification::error(e).title("Invalid connection"), window, cx);
                return;
            }
        };
        if !self.dialog.store_password {
            config.password = None;
        }
        let name = config.name.clone();
        self.connections.push(config);
        store::save_connections(&self.connections);
        self.log_console(true, "save connection", &name);
        self.toast(
            Notification::success(format!("Saved connection {name}")),
            window,
            cx,
        );
        self.close_conn_dialog(cx);
    }

    /// Try opening the connection, then report — never touches workspace
    /// state, the driver is dropped right after (which closes the session).
    fn dialog_test(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.conn_testing {
            return;
        }
        let config = match self.dialog_build_config(cx) {
            Ok(c) => Arc::new(c),
            Err(e) => {
                self.toast(Notification::error(e).title("Invalid connection"), window, cx);
                return;
            }
        };
        self.conn_testing = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let cfg = Arc::clone(&config);
            let outcome = sqlpro_drivers::runtime::on_thread(move || {
                sqlpro_drivers::create_driver(&cfg).map(|driver| driver.label().to_string())
            })
            .await;
            // The Box is dropped at the end of on_thread's task: closed.
            let msg = match outcome {
                Ok(label) => Ok(label),
                Err(e) => Err(e.to_string()),
            };
            _ = this.update_in(cx, |ws, window, cx| {
                ws.conn_testing = false;
                match msg {
                    Ok(label) => ws.toast(
                        Notification::success(format!("Test succeeded: {label}"))
                            .title("Connection test"),
                        window,
                        cx,
                    ),
                    Err(e) => ws.toast(
                        Notification::error(e).title("Connection test failed"),
                        window,
                        cx,
                    ),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Save the connection and open it.
    fn dialog_connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let config = match self.dialog_build_config(cx) {
            Ok(c) => c,
            Err(e) => {
                self.toast(Notification::error(e).title("Invalid connection"), window, cx);
                return;
            }
        };
        if self.running {
            self.toast(
                Notification::warning("Busy — wait for the current operation"),
                window,
                cx,
            );
            return;
        }
        // What lands on disk respects "Store in keychain": without it the
        // password stays only in the in-memory config for this connect.
        let mut saved = config.clone();
        if !self.dialog.store_password {
            saved.password = None;
        }
        self.connections.push(saved);
        store::save_connections(&self.connections);
        self.close_conn_dialog(cx);
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

            _ = this.update_in(cx, |ws, window, cx| {
                ws.running = false;
                match outcome {
                    Ok(driver) => {
                        ws.log_console(
                            true,
                            &format!("connect {}", config.name),
                            &format!("{} tables", tables.len()),
                        );
                        ws.status = format!("Connected: {}", driver.label());
                        ws.toast(
                            Notification::success(format!(
                                "Connected: {} — {} tables",
                                driver.label(),
                                tables.len()
                            )),
                            window,
                            cx,
                        );
                        ws.driver = Some(driver);
                        ws.active = Some(config);
                        ws.tables = tables;
                    }
                    Err(e) => {
                        ws.log_console(false, &format!("connect {}", config.name), &e);
                        ws.status = format!("Connection failed: {e}");
                        ws.last_error = Some(e.clone());
                        ws.toast(
                            Notification::error(e).title("Connection failed"),
                            window,
                            cx,
                        );
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Close the active connection and return to the welcome screen. With
    /// staged changes present, the first press asks for confirmation.
    fn disconnect(&mut self, cx: &mut Context<Self>) {
        if self.running {
            self.status = "Busy — wait for the current operation to finish".into();
            cx.notify();
            return;
        }
        if !self.pending.is_empty() && !self.close_confirmed {
            self.close_confirmed = true;
            self.status = format!(
                "{} staged change(s) will be discarded — press close again to confirm",
                self.pending.changes.len()
            );
            cx.notify();
            return;
        }
        self.close_confirmed = false;
        let name = self
            .active
            .as_ref()
            .map(|c| c.name.clone())
            .unwrap_or_default();
        self.driver = None;
        self.active = None;
        self.tables.clear();
        self.active_table = None;
        self.table_name.clear();
        self.pk_columns.clear();
        self.structure.clear();
        self.columns.clear();
        self.original_rows.clear();
        self.rows.clear();
        self.row_ids.clear();
        self.result_row_total = None;
        self.last_result = None;
        self.last_error = None;
        self.pending.clear();
        self.editing_cell = None;
        self.grid_mode = GridMode::Data;
        self.table.update(cx, |table, cx| {
            *table.delegate_mut() = ResultsDelegate::empty();
            table.refresh(cx);
        });
        self.log_console(true, &format!("disconnect {name}"), "closed");
        self.status = format!("Closed connection {name}");
        cx.notify();
    }

    // ---- Database switcher (same server) --------------------------------

    /// List the other databases on the current server, using the active
    /// connection's credentials. PostgreSQL and MySQL only.
    fn discover_databases(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.db_switch_open {
            self.db_switch_open = false;
            cx.notify();
            return;
        }
        let Some(driver) = self.driver.clone() else { return };
        let engine = self
            .active
            .as_ref()
            .map(|c| c.database_type.clone())
            .unwrap_or_else(|| DatabaseType::new(DatabaseType::SQLITE));
        let query = match engine.driver_id() {
            "postgresql" => {
                "SELECT datname FROM pg_database WHERE datistemplate = false ORDER BY 1"
                    .to_string()
            }
            "mysql" => "SHOW DATABASES".to_string(),
            _ => {
                self.status =
                    "Database switching is not supported for this engine".into();
                cx.notify();
                return;
            }
        };

        self.db_switch_open = true;
        self.db_switch_loading = true;
        self.discovered_dbs.clear();
        cx.notify();

        cx.spawn_in(window, async move |this, cx| {
            let result =
                sqlpro_drivers::runtime::on_tokio(async move { driver.execute(&query).await })
                    .await;
            _ = this.update(cx, |ws, cx| {
                ws.db_switch_loading = false;
                match result {
                    Ok(r) => {
                        let mut dbs: Vec<String> = r
                            .rows
                            .iter()
                            .filter_map(|row| row.first())
                            .filter_map(|c| match c {
                                CellValue::Text(s) => Some(s.clone()),
                                _ => None,
                            })
                            .collect();
                        if engine.driver_id() == "mysql" {
                            let system = ["information_schema", "mysql", "performance_schema", "sys"];
                            dbs.retain(|d| !system.contains(&d.as_str()));
                        }
                        let current =
                            ws.active.as_ref().and_then(|c| c.database.clone());
                        dbs.retain(|d| Some(d) != current.as_ref());
                        ws.discovered_dbs = dbs;
                    }
                    Err(e) => {
                        ws.status = format!("Database discovery failed: {e}");
                        ws.db_switch_open = false;
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Reconnect with the same credentials to a different database on the
    /// same server. Not persisted to the saved-connection list.
    fn reconnect_to_database(
        &mut self,
        db: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(active) = self.active.clone() else { return };
        let mut config = (*active).clone();
        config.database = Some(db.to_string());
        self.db_switch_open = false;
        self.status = format!("Switching to {db}…");
        cx.notify();
        self.connect(Arc::new(config), None, window, cx);
    }

    // ---- Queries -------------------------------------------------------

    /// Surface a one-shot outcome (save/export results, copy confirmations,
    /// "select a cell first" hints) as a toast — the 26px status bar alone
    /// is too easy to miss.
    fn toast(&self, note: Notification, window: &mut Window, cx: &mut Context<Self>) {
        window.push_notification(note, cx);
    }

    fn run_query(&mut self, _: &RunQuery, window: &mut Window, cx: &mut Context<Self>) {
        if self.running {
            return;
        }
        let Some(driver) = self.driver.clone() else {
            self.status = "Not connected".into();
            self.toast(Notification::warning("Not connected — choose a connection first"), window, cx);
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

            _ = this.update_in(cx, |ws, window, cx| {
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
                        eprintln!("query failed: {e}");
                        ws.status = format!("Error: {e}");
                        ws.last_error = Some(e.to_string());
                        ws.toast(
                            Notification::error(e.to_string()).title("Query failed"),
                            window,
                            cx,
                        );
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
                editing: None,
                editor: Some(self.cell_editor.clone()),
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

    /// Stage a new value for one cell: record the change in PendingChanges
    /// (visible as a tint, not written) and update the displayed row.
    fn stage_cell_value(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        new_text: String,
        cx: &mut Context<Self>,
    ) {
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
                    *cell = new_value.clone();
                }
            }
        }
        self.status = format!(
            "Staged: {} = {} — ⌘S writes to the database",
            column.name,
            match &new_value {
                CellValue::Null => "NULL".to_string(),
                CellValue::Text(t) => t.clone(),
                CellValue::Bytes(_) => "<binary>".to_string(),
            }
        );
        self.refresh_grid(cx);
        cx.notify();
    }

    /// Commit the inline cell editor's value into the staged changes and
    /// leave editing mode.
    fn commit_cell_edit(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some((row_ix, col_ix)) = self.editing_cell else { return };
        self.editing_cell = None;
        let new_text = self.cell_editor.read(cx).value().to_string();
        self.stage_cell_value(row_ix, col_ix, new_text, cx);
        self.sync_delegate_editing(cx);
    }

    /// Cancel inline editing without staging anything (Escape).
    fn cancel_cell_edit(&mut self, cx: &mut Context<Self>) {
        if self.editing_cell.take().is_some() {
            self.status = "Cell edit cancelled".into();
            self.sync_delegate_editing(cx);
            cx.notify();
        }
    }

    /// Push the current editing cell + editor entity into the grid delegate
    /// so `render_td` draws the editor inside the cell.
    fn sync_delegate_editing(&mut self, cx: &mut Context<Self>) {
        let editing = self.editing_cell;
        let editor = Some(self.cell_editor.clone());
        self.table.update(cx, |table, cx| {
            table.delegate_mut().editing = editing;
            table.delegate_mut().editor = editor;
            table.refresh(cx);
        });
    }

    /// "Edit cell" from the row context menu (or a double-click): render
    /// the editor inside the cell and focus it. Enter stages, ⌘S saves.
    fn begin_cell_edit(
        &mut self,
        cell: Option<(usize, usize)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = cell.or_else(|| self.selected_cell(cx));
        let Some((row_ix, col_ix)) = target else {
            self.status = "Select a cell first".into();
            self.toast(Notification::warning("Select a cell first"), window, cx);
            cx.notify();
            return;
        };
        if self.editing_cell.is_some_and(|c| c == (row_ix, col_ix)) {
            return;
        }
        // A previous inline edit that never saw Enter is staged on blur
        // before the new cell takes over.
        if self.editing_cell.is_some() {
            self.commit_cell_edit(window, cx);
        }
        let value = self
            .rows
            .get(row_ix)
            .and_then(|r| r.get(col_ix))
            .map(|c| match c {
                CellValue::Null => String::new(),
                other => other.display(),
            })
            .unwrap_or_default();
        let column = self
            .columns
            .get(col_ix)
            .map(|c| c.name.clone())
            .unwrap_or_else(|| "cell".into());

        self.editing_cell = Some((row_ix, col_ix));
        self.cell_editor.update(cx, |state, cx| {
            state.set_value(value, window, cx);
            state.focus(window, cx);
        });
        self.sync_delegate_editing(cx);
        self.status = format!("Editing {column} — ⏎ to stage, ⌘S to save");
        cx.notify();
    }

    fn copy_cell_value(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((row_ix, col_ix)) = self.selected_cell(cx) else {
            self.status = "Select a cell first".into();
            self.toast(Notification::warning("Select a cell first"), window, cx);
            cx.notify();
            return;
        };
        let text = self
            .rows
            .get(row_ix)
            .and_then(|r| r.get(col_ix))
            .map(|c| c.display())
            .unwrap_or_default();
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
        self.status = "Cell copied".into();
        self.toast(Notification::success("Cell copied"), window, cx);
        cx.notify();
    }

    // ---- Context-menu action handlers -----------------------------------

    fn edit_cell_action(
        &mut self,
        _: &EditCell,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.begin_cell_edit(None, window, cx);
    }

    fn copy_cell_action(&mut self, _: &CopyCell, window: &mut Window, cx: &mut Context<Self>) {
        self.copy_cell_value(window, cx);
    }

    fn copy_row_json_action(&mut self, _: &CopyRowJson, window: &mut Window, cx: &mut Context<Self>) {
        self.copy_row_as(RowCopyFormat::Json, window, cx);
    }

    fn copy_row_yaml_action(&mut self, _: &CopyRowYaml, window: &mut Window, cx: &mut Context<Self>) {
        self.copy_row_as(RowCopyFormat::Yaml, window, cx);
    }

    fn copy_row_csv_action(&mut self, _: &CopyRowCsv, window: &mut Window, cx: &mut Context<Self>) {
        self.copy_row_as(RowCopyFormat::Csv, window, cx);
    }

    fn insert_row_action(&mut self, _: &InsertRow, window: &mut Window, cx: &mut Context<Self>) {
        self.insert_row(window, cx);
    }

    fn delete_row_action(&mut self, _: &DeleteRow, window: &mut Window, cx: &mut Context<Self>) {
        self.delete_row(window, cx);
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

    fn insert_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.columns.is_empty() {
            self.status = "Open a table first".into();
            self.toast(Notification::warning("Open a table first"), window, cx);
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

    pub(crate) fn delete_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row_ix) = self.selected_row(cx) else {
            self.status = "Select a row first".into();
            self.toast(Notification::warning("Select a row first"), window, cx);
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

    pub(crate) fn save_changes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // An inline edit still open — typed but not confirmed with Enter —
        // is staged first, so ⌘S applies exactly what's on screen.
        if self.editing_cell.is_some() {
            self.commit_cell_edit(window, cx);
        }
        if self.pending.is_empty() {
            self.status = "Nothing to save".into();
            self.toast(
                Notification::info("Nothing to save — edit a cell, or insert/delete a row first"),
                window,
                cx,
            );
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
            let outcome: std::result::Result<(DataWriteResult, Vec<String>), String> =
                sqlpro_drivers::runtime::on_tokio(async move {
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
                    if attributed.is_empty() {
                        // Silently executing nothing would look like a
                        // successful save; surface why instead.
                        return Err(
                            "no SQL could be generated for the staged changes — the row's \
                             key value is missing (NULL primary key?), so the edit cannot be \
                             matched. Reload the table and retry."
                                .to_string(),
                        );
                    }
                    let sql_texts: Vec<String> = attributed
                        .iter()
                        .map(|a| a.statement.sql.clone())
                        .collect();
                    let plan =
                        DataWritePlan::from_attributed(attributed, has_pk, TransactionOwner::App);
                    let executor = DataWriteExecutor { driver: driver.as_ref(), real_counts };
                    executor
                        .run(&plan)
                        .await
                        .map(|result| (result, sql_texts))
                        .map_err(|e| e.to_string())
                })
                .await;

            _ = this.update_in(cx, |ws, window, cx| {
                match outcome {
                    Ok((result, sqls)) => {
                        let affected: usize =
                            result.executed.iter().map(|s| s.rows_affected as usize).sum();
                        for (step, sql) in result.executed.iter().zip(&sqls) {
                            ws.log_console(
                                true,
                                sql,
                                &format!("{} row(s) affected", step.rows_affected),
                            );
                            eprintln!("save ok: {sql}");
                        }
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
                        ws.toast(
                            Notification::success(format!(
                                "{} statement(s), {affected} row(s) affected",
                                result.executed.len()
                            ))
                            .title("Save complete"),
                            window,
                            cx,
                        );
                        ws.pending.clear();
                        ws.reload_current_table(window, cx);
                    }
                    Err(e) => {
                        ws.log_console(false, "commit", &e.clone());
                        eprintln!("save failed: {e}");
                        ws.status = format!("Save failed: {e}");
                        ws.last_error = Some(e.clone());
                        ws.toast(
                            Notification::error(e).title("Save failed"),
                            window,
                            cx,
                        );
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

    fn revert_changes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
        self.toast(
            Notification::info("Reverted all staged changes"),
            window,
            cx,
        );
        cx.notify();
    }

    // ---- Export --------------------------------------------------------

    fn export_csv(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.rows.is_empty() {
            self.status = "No rows to export".into();
            self.toast(Notification::warning("No rows to export"), window, cx);
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
                self.toast(
                    Notification::success(format!(
                        "{} rows written to {}",
                        self.rows.len(),
                        path.display()
                    ))
                    .title("Export complete"),
                    window,
                    cx,
                );
            }
            Err(e) => {
                self.status = format!("Export failed: {e}");
                self.toast(
                    Notification::error(e.to_string()).title("Export failed"),
                    window,
                    cx,
                );
            }
        }
        cx.notify();
    }

    // ---- Row copying (inspector) ---------------------------------------

    /// Copy only the currently selected row to the clipboard in the given
    /// format (JSON object, YAML document, or CSV with a header line).
    fn copy_row_as(
        &mut self,
        format: RowCopyFormat,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(row_ix) = self.selected_row(cx) else {
            self.status = "No row selected — select a row to copy".into();
            self.toast(
                Notification::warning("No row selected — select a row to copy"),
                window,
                cx,
            );
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
        self.toast(
            Notification::success(format!(
                "Row {} copied as {}",
                row_ix + 1,
                format.label()
            )),
            window,
            cx,
        );
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

    /// Push the current selection back into the table after a delegate
    /// rebuild, so Apply/row operations keep working after a stage refresh.
    fn restore_selection(&mut self, cx: &mut Context<Self>) {
        let selection = self.table.read(cx).selection();
        let row_count = self.rows.len();
        if let Some((row, col)) = match selection {
            gpui_kit::component::table::TableSelection::Cell(r, c) => Some((r, c)),
            gpui_kit::component::table::TableSelection::Row(r) => Some((r, 0)),
            _ => None,
        } {
            if row < row_count && col < self.columns.len() {
                self.table.update(cx, |table, cx| {
                    table.set_selected_cell(row, col, cx);
                });
            }
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
            *table.delegate_mut() = ResultsDelegate {
                columns,
                rows,
                visual,
                editing: self.editing_cell,
                editor: Some(self.cell_editor.clone()),
            };
            table.refresh(cx);
        });
        self.restore_selection(cx);
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
        if let Some(entry) = catalog_entry(db) {
            return entry.label;
        }
        match db.driver_id() {
            "postgresql" => "PostgreSQL",
            "mysql" => "MySQL",
            "sqlite" => "SQLite",
            _ => "Database",
        }
    }

    /// The circular engine badge on the start screen: letters + color.
    fn engine_badge(db: &DatabaseType) -> (&'static str, u32) {
        if let Some(entry) = catalog_entry(db) {
            return (entry.letters, entry.color);
        }
        match db.driver_id() {
            "postgresql" => ("Pg", 0x2F7CF6),
            "mysql" => ("My", 0xE68A2E),
            "sqlite" => ("Sq", 0x3FB950),
            _ => ("Db", 0x8B909A),
        }
    }

    /// A small environment chip for a connection. An explicit tag set in
    /// the dialog wins; otherwise derive one ("demo", "local", "remote").
    fn connection_tag(config: &ConnectionConfig) -> Option<(String, u32)> {
        if let Some(tag) = &config.tag {
            return Some((tag.clone(), 0x8957E5));
        }
        if config.name.contains("Demo") {
            return Some(("demo".into(), 0x8957E5));
        }
        match config.host.as_deref() {
            Some("127.0.0.1") | Some("localhost") => Some(("local".into(), 0x3FB950)),
            Some(_) => Some(("remote".into(), 0x2F7CF6)),
            None => None,
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
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        // No connection yet (and none in progress with a driver): show the
        // welcome/start screen instead of the four-pane workspace.
        if self.driver.is_none() {
            return self.render_start_screen(cx).into_any_element();
        }

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
            .on_action(cx.listener(Self::edit_cell_action))
            .on_action(cx.listener(Self::copy_cell_action))
            .on_action(cx.listener(Self::copy_row_json_action))
            .on_action(cx.listener(Self::copy_row_yaml_action))
            .on_action(cx.listener(Self::copy_row_csv_action))
            .on_action(cx.listener(Self::insert_row_action))
            .on_action(cx.listener(Self::delete_row_action))
            // Direct keyboard shortcuts on the root container. Handled in
            // the bubble phase so focused inputs keep their own keys;
            // matching is case-insensitive because macOS may deliver "S"
            // or "s" for the ⌘S keystroke.
            .on_key_down({
                let entity = cx.entity();
                move |event: &KeyDownEvent, window: &mut Window, cx: &mut App| {
                    let keystroke = event.keystroke.key.to_lowercase();
                    let cmd = if cfg!(target_os = "macos") {
                        event.keystroke.modifiers.platform
                    } else {
                        event.keystroke.modifiers.control
                    };
                    match (keystroke.as_str(), cmd) {
                        ("s", true) => entity.update(cx, |ws, ws_cx| {
                            eprintln!(
                                "shortcut: cmd-s -> save (pending={})",
                                ws.pending.changes.len()
                            );
                            ws.save_changes(window, ws_cx);
                        }),
                        ("enter", true) => entity.update(cx, |ws, ws_cx| {
                            eprintln!("shortcut: cmd-enter -> run query");
                            ws.run_query(&RunQuery, window, ws_cx);
                        }),
                        ("e", true) | ("t", true) => entity.update(cx, |ws, ws_cx| {
                            ws.new_query(window, ws_cx);
                        }),
                        ("escape", false) => entity.update(cx, |ws, ws_cx| {
                            if ws.dialog_open {
                                ws.close_conn_dialog(ws_cx);
                            } else if ws.db_switch_open {
                                ws.db_switch_open = false;
                                ws_cx.notify();
                            } else {
                                ws.cancel_cell_edit(ws_cx);
                            }
                        }),
                        _ => {}
                    }
                }
            })
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
                    // Live indicator while a query/save/connect is in flight
                    .when(self.running, |bar| {
                        bar.child(
                            Spinner::new().xsmall().color(color(COLOR_ACCENT)),
                        )
                    })
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
            // Database switcher popover: full-window backdrop + menu
            // anchored under the connection pill (painted last = on top).
            .when(self.db_switch_open, |root| {
                let current = self.active.as_ref().and_then(|c| c.database.clone());
                root.child(
                    div()
                        .id("db-switch-backdrop")
                        .absolute()
                        .inset_0()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.db_switch_open = false;
                            cx.notify();
                        })),
                )
                .child(
                    v_flex()
                        .id("db-switch-menu")
                        // Rows must not also fire the backdrop's close.
                        .occlude()
                        .absolute()
                        .left(px(12.))
                        .top(px(52.))
                        .w(px(260.))
                        .max_h(px(300.))
                        .overflow_y_scroll()
                        .rounded(px(6.))
                        .border_1()
                        .border_color(border)
                        .bg(color(COLOR_SURFACE2))
                        .shadow_lg()
                        .p_1()
                        .when(self.db_switch_loading, |menu| {
                            menu.child(
                                div()
                                    .px_2()
                                    .py_1p5()
                                    .text_size(px(11.5))
                                    .text_color(color(COLOR_MUTED))
                                    .child("Discovering databases…"),
                            )
                        })
                        .when(
                            !self.db_switch_loading && self.discovered_dbs.is_empty(),
                            |menu| {
                                menu.child(
                                    div()
                                        .px_2()
                                        .py_1p5()
                                        .text_size(px(11.5))
                                        .text_color(color(COLOR_MUTED))
                                        .child("No other databases found"),
                                )
                            },
                        )
                        .children(self.discovered_dbs.iter().map(|db_name| {
                            let is_current = current.as_ref() == Some(db_name);
                            let db = db_name.clone();
                            div()
                                .id(SharedString::from(format!("db-{db}")))
                                .h(px(26.))
                                .flex_shrink_0()
                                .flex()
                                .items_center()
                                .gap_2()
                                .px_2()
                                .rounded(px(4.))
                                .text_size(px(12.))
                                .hover(|this| this.bg(color(COLOR_HOVER)))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.reconnect_to_database(&db, window, cx)
                                }))
                                .child(db_name.clone())
                                .when(is_current, |row| {
                                    row.child(
                                        div()
                                            .text_size(px(9.5))
                                            .text_color(color(COLOR_MUTED))
                                            .child("current"),
                                    )
                                })
                        })),
                )
            })
            // New-connection dialog paints over everything.
            .when(self.dialog_open, |root| {
                root.child(self.conn_dialog_overlay(cx))
            })
            .into_any_element()
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
    // ---- Start screen (no connection yet) ----

    /// The welcome screen shown while no database is connected: branding
    /// and quick actions on the left, saved connections on the right with
    /// engine badges, environment tags, and a create row at the bottom.
    fn render_start_screen(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let border = color(COLOR_BORDER);
        let error = self.last_error.clone();
        let status = self.status.clone();
        let connecting = self.running && self.driver.is_none();

        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .bg(color(COLOR_BG))
            .text_color(color(COLOR_TEXT))
            .text_size(px(13.))
            // Escape dismisses the connection dialog from the welcome screen
            // (the workspace root handles it once connected).
            .on_key_down({
                let entity = cx.entity();
                move |event: &KeyDownEvent, _: &mut Window, cx: &mut App| {
                    if event.keystroke.key.to_lowercase() == "escape" {
                        entity.update(cx, |ws, cx| {
                            if ws.dialog_open {
                                ws.close_conn_dialog(cx);
                            }
                        });
                    }
                }
            })
            .child(
                h_flex()
                    .w(px(680.))
                    .h(px(480.))
                    .overflow_hidden()
                    .rounded(px(10.))
                    .border_1()
                    .border_color(border)
                    .shadow_lg()
                    // Left panel: branding + quick actions
                    .child(
                        v_flex()
                            .w(px(260.))
                            .flex_shrink_0()
                            .h_full()
                            .items_center()
                            .bg(color(COLOR_SURFACE))
                            .border_r_1()
                            .border_color(border)
                            .child(div().flex_1())
                            .child(
                                Icon::new(IconName::Database)
                                    .size(px(52.))
                                    .text_color(color(COLOR_ACCENT)),
                            )
                            .child(
                                div()
                                    .mt_4()
                                    .text_size(px(20.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .child("Welcome to SQL Pro"),
                            )
                            .child(
                                div()
                                    .mt_1()
                                    .text_size(px(11.))
                                    .text_color(color(COLOR_MUTED))
                                    .child("Version 0.1 · PostgreSQL · MySQL · SQLite"),
                            )
                            .when(!status.is_empty() && status != "Ready", |this| {
                                this.child(
                                    div()
                                        .mt_3()
                                        .max_w(px(260.))
                                        .text_align(TextAlign::Center)
                                        .text_size(px(11.))
                                        .when_some(error.clone(), |this, e| {
                                            this.text_color(color(COLOR_RED)).child(e)
                                        })
                                        .when(error.is_none(), |this| {
                                            this.text_color(color(COLOR_MUTED)).child(status.clone())
                                        }),
                                )
                            })
                            .child(div().flex_1())
                            .child(
                                h_flex()
                                    .gap_2()
                                    .pb(px(28.))
                                    .child(
                                        Button::new("start-new-conn")
                                            .small()
                                            .outline()
                                            .child(
                                                h_flex()
                                                    .gap_1()
                                                    .child(
                                                        div()
                                                            .text_size(px(12.))
                                                            .line_height(px(1.))
                                                            .child("+"),
                                                    )
                                                    .child("Connection"),
                                            )
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.open_conn_dialog(window, cx)
                                            })),
                                    )
                                    .child(
                                        Button::new("start-demo")
                                            .small()
                                            .ghost()
                                            .disabled(connecting)
                                            .child(
                                                h_flex()
                                                    .gap_1()
                                                    .child(
                                                        div()
                                                            .text_size(px(12.))
                                                            .line_height(px(1.))
                                                            .child("▤"),
                                                    )
                                                    .child("Demo DB"),
                                            )
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.connect_demo(window, cx)
                                            })),
                                    ),
                            ),
                    )
                    // Right panel: saved connections
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .child(
                                v_flex()
                                    .id("start-connections")
                                    .flex_1()
                                    .min_h_0()
                                    .overflow_y_scroll()
                                    .py_2()
                                    .children(
                                        self.connections
                                            .iter()
                                            .enumerate()
                                            .map(|(ix, config)| {
                                                self.start_connection_row(ix, config, cx)
                                            }),
                                    )
                                    .when(self.connections.is_empty(), |this| {
                                        this.child(
                                            div()
                                                .p_6()
                                                .text_size(px(12.))
                                                .text_color(color(COLOR_MUTED))
                                                .child("No saved connections yet"),
                                        )
                                    }),
                            )
                            .child(
                                div()
                                    .id("start-create-connection")
                                    .h(px(44.))
                                    .flex_shrink_0()
                                    .flex()
                                    .items_center()
                                    .px_5()
                                    .border_t_1()
                                    .border_color(border)
                                    .text_size(px(12.5))
                                    .text_color(color(COLOR_MUTED))
                                    .hover(|this| this.text_color(color(COLOR_ACCENT)))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.open_conn_dialog(window, cx)
                                    }))
                                    .child("Create a new connection…"),
                            ),
                    ),
            )
            // The connection dialog paints over the welcome card.
            .when(self.dialog_open, |root| {
                root.child(self.conn_dialog_overlay(cx))
            })
    }

    fn start_connection_row(
        &self,
        ix: usize,
        config: &ConnectionConfig,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let is_active = self.active.as_ref().is_some_and(|a| a.id == config.id);
        let connected = is_active && self.driver.is_some();
        let connecting = is_active && self.running && self.driver.is_none();
        let (badge_letters, badge_color) = Self::engine_badge(&config.database_type);
        let tag = Self::connection_tag(config);
        let summary = Self::connection_summary(config);
        let name = config.name.clone();

        div()
            .id(SharedString::from(format!("start-conn-{ix}")))
            .h(px(44.))
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap_2p5()
            .px_4()
            .when(is_active, |this| this.bg(color(COLOR_HOVER)))
            .hover(|this| this.bg(color(COLOR_HOVER)))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.connect_saved(ix, window, cx)
            }))
            .child(
                div()
                    .w(px(28.))
                    .h(px(28.))
                    .flex_shrink_0()
                    .rounded_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(color(badge_color))
                    .text_color(gpui::white())
                    .text_size(px(10.5))
                    .font_weight(FontWeight::MEDIUM)
                    .child(badge_letters),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
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
                                    .text_size(px(13.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(name),
                            )
                            .when_some(tag, |row, (label, c)| {
                                row.child(
                                    div()
                                        .px_1()
                                        .rounded(px(3.))
                                        .flex_shrink_0()
                                        .text_size(px(9.))
                                        .font_weight(FontWeight::MEDIUM)
                                        .bg(gpui::rgba((c << 8) | 0x29))
                                        .text_color(color(c))
                                        .child(label),
                                )
                            }),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_size(px(10.5))
                            .text_color(color(COLOR_MUTED))
                            .child(summary),
                    ),
            )
            .when(connected || connecting, |this| {
                this.child(self.colored_dot(
                    if connected {
                        ConnState::Connected
                    } else {
                        ConnState::Connecting
                    },
                    config.color,
                ))
            })
    }

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
            // Discover other databases on the same server (opens the
            // switcher popover under the pill).
            .child(
                self.glyph_button("switch-db", "⇄", self.db_switch_open)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.discover_databases(window, cx)
                    })),
            )
            .child(div().w(px(1.)).h(px(22.)).bg(border).mx_1())
            // Refresh
            .child(
                self.glyph_button("refresh", "⟳", false).on_click(
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
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .line_height(px(1.))
                                    .child("✓"),
                            )
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
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .line_height(px(1.))
                                    .child("↩"),
                            )
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
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .line_height(px(1.))
                                    .child("⇓"),
                            )
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
                                div()
                                    .text_size(px(12.))
                                    .line_height(px(1.))
                                    .text_color(self.safe_mode.color())
                                    .child(match self.safe_mode {
                                        SafeMode::Off => "○",
                                        SafeMode::Warn => "◐",
                                        SafeMode::Strict => "●",
                                    }),
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
            // Console toggle — "▄" (lower half block) reads as "bottom
            // panel", mirroring "◨" for the right inspector.
            .child(
                self.glyph_button("toggle-console", "▄", self.show_console)
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
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .line_height(px(1.))
                                    .child("+"),
                            )
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_conn_dialog(window, cx)
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
                            .child(self.colored_dot(
                                if is_active {
                                    ConnState::Connected
                                } else {
                                    ConnState::Offline
                                },
                                config.color,
                            ))
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
            .relative()
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
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .line_height(px(1.))
                                    .child("⊞"),
                            )
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
                                    .child(
                                        div()
                                            .text_size(px(11.))
                                            .line_height(px(1.))
                                            .child("▶"),
                                    )
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
                            .child(
                                h_flex()
                                    .gap_1()
                                    .child(
                                        div()
                                            .text_size(px(11.))
                                            .line_height(px(1.))
                                            .child("▶▶"),
                                    )
                                    .child("Run all"),
                            )
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
                        .id("grid-data-area")
                        .flex_1()
                        .min_h_0()
                        .overflow_hidden()
                        .bg(color(COLOR_BG))
                        .child(self.table.clone())
                        .context_menu(|menu, _window, _cx| {
                            menu.menu("Edit cell", Box::new(EditCell))
                                .menu("Copy cell", Box::new(CopyCell))
                                .separator()
                                .menu("Copy row as JSON", Box::new(CopyRowJson))
                                .menu("Copy row as YAML", Box::new(CopyRowYaml))
                                .menu("Copy row as CSV", Box::new(CopyRowCsv))
                                .separator()
                                .menu("Insert row", Box::new(InsertRow))
                                .menu("Delete row", Box::new(DeleteRow))
                        })
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
            .child(self.grid_mode_button("grid-data", "▦", "Data", GridMode::Data, cx))
            .child(self.grid_mode_button(
                "grid-structure",
                "☰",
                "Structure",
                GridMode::Structure,
                cx,
            ))
            .child(div().w(px(1.)).h(px(16.)).bg(color(COLOR_BORDER_SOFT)).mx_1())
            // Row operations — cells are edited inline on the grid
            // (double-click or right-click → Edit cell).
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
        glyph: &'static str,
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
            .gap_1()
            .px_2()
            .rounded(px(4.))
            .text_size(px(11.5))
            .font_weight(if active { FontWeight::MEDIUM } else { FontWeight::NORMAL })
            .text_color(if active { color(COLOR_TEXT) } else { color(COLOR_MUTED) })
            .when(active, |this| this.bg(color(COLOR_HOVER)))
            .hover(|this| this.text_color(color(COLOR_TEXT)))
            .child(glyph)
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
            .disabled(!has_row)
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_size(px(11.))
                            .line_height(px(1.))
                            .child("⧉"),
                    )
                    .child(format.label()),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.copy_row_as(format, window, cx)
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

    // ---- New-connection dialog rendering --------------------------------

    /// The modal overlay: dimmed backdrop (click to cancel) with the dialog
    /// card centered on top. Painted last on both the welcome screen and the
    /// workspace root, so it floats over either.
    fn conn_dialog_overlay(&self, cx: &mut Context<Self>) -> Div {
        div().absolute().inset_0().child(
            div()
                .id("conn-dialog-backdrop")
                .absolute()
                .inset_0()
                .bg(gpui::rgba(0x00000059))
                .on_click(cx.listener(|this, _, _, cx| this.close_conn_dialog(cx))),
        )
        .child(
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .child(match self.dialog.step {
                    ConnDialogStep::PickType => self.render_engine_picker(cx).into_any_element(),
                    ConnDialogStep::Details => self.render_conn_details(cx).into_any_element(),
                    ConnDialogStep::ImportUrl => self.render_url_import(cx).into_any_element(),
                }),
        )
    }

    /// A right-aligned form label for the details rows.
    fn dialog_label(text: &str) -> Div {
        div()
            .w(px(92.))
            .flex_shrink_0()
            .text_right()
            .text_size(px(12.))
            .text_color(color(COLOR_MUTED))
            .child(text.to_string())
    }

    /// Step 1 — the engine grid: circular badges in a 4×3 catalog, Cancel /
    /// Import from URL / Create in the footer (screenshot 1).
    fn render_engine_picker(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let border = color(COLOR_BORDER);
        let selected = self.dialog.engine_ix;

        let mut grid = v_flex().gap_1p5();
        for (row_ix, row) in ENGINE_CATALOG.chunks(4).enumerate() {
            let cell_list: Vec<_> = row
                .iter()
                .enumerate()
                .map(|(col_ix, entry)| {
                    let ix = row_ix * 4 + col_ix;
                    let is_selected = selected == Some(ix);
                    v_flex()
                        .id(SharedString::from(format!("engine-{}", entry.id)))
                        .w(px(108.))
                        .h(px(84.))
                        .flex_shrink_0()
                        .items_center()
                        .justify_center()
                        .gap_1p5()
                        .rounded(px(8.))
                        .border_1()
                        .border_color(if is_selected {
                            color(COLOR_ACCENT)
                        } else {
                            gpui::hsla(0., 0., 0., 0.)
                        })
                        .when(is_selected, |cell| cell.bg(color(COLOR_HOVER)))
                        .hover(|cell| cell.bg(color(COLOR_HOVER)))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.dialog_pick_engine(ix, cx)
                        }))
                        .child(
                            div()
                                .w(px(38.))
                                .h(px(38.))
                                .rounded_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .bg(gpui::rgb(entry.color))
                                .text_color(gpui::white())
                                .text_size(px(13.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(entry.letters),
                        )
                        .child(
                            div()
                                .max_w(px(96.))
                                .text_align(TextAlign::Center)
                                .text_size(px(10.5))
                                .line_height(px(1.25))
                                .text_color(color(COLOR_TEXT))
                                .child(entry.label),
                        )
                })
                .collect();
            grid = grid.child(h_flex().gap_1p5().children(cell_list));
        }

        v_flex()
            .w(px(476.))
            // Clicks inside the card must not reach the close-on-click
            // backdrop behind it (gpui dispatches to every hovered hitbox).
            .occlude()
            .rounded(px(10.))
            .border_1()
            .border_color(border)
            .bg(color(COLOR_SURFACE))
            .shadow_lg()
            .overflow_hidden()
            .child(
                v_flex()
                    .p_4()
                    .gap_2()
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(color(COLOR_MUTED))
                            .child("CHOOSE A DATABASE TYPE"),
                    )
                    .child(grid),
            )
            .child(
                h_flex()
                    .justify_between()
                    .px_4()
                    .py_3()
                    .border_t_1()
                    .border_color(border)
                    .bg(color(COLOR_SURFACE2))
                    .child(
                        Button::new("dialog-cancel")
                            .small()
                            .ghost()
                            .child(
                                h_flex()
                                    .gap_1()
                                    .child(
                                        div().text_size(px(12.)).line_height(px(1.)).child("✕"),
                                    )
                                    .child("Cancel"),
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.close_conn_dialog(cx)
                            })),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("dialog-import-url")
                                    .small()
                                    .outline()
                                    .child(
                                        h_flex()
                                            .gap_1()
                                            .child(
                                                div()
                                                    .text_size(px(12.))
                                                    .line_height(px(1.))
                                                    .child("⇩"),
                                            )
                                            .child("Import from URL"),
                                    )
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.dialog.step = ConnDialogStep::ImportUrl;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("dialog-create")
                                    .small()
                                    .primary()
                                    .disabled(self.dialog.engine_ix.is_none())
                                    .child(
                                        h_flex()
                                            .gap_1()
                                            .child(
                                                div()
                                                    .text_size(px(12.))
                                                    .line_height(px(1.))
                                                    .child("+"),
                                            )
                                            .child("Create"),
                                    )
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.dialog_advance(cx)
                                    })),
                            ),
                    ),
            )
    }

    /// Step 2 — the details form (screenshot 2): name, status color + tag,
    /// host/port, user, password (+ store in keychain), database, SSL,
    /// optional SSH section, footer Save / Test / Connect.
    fn render_conn_details(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let border = color(COLOR_BORDER);
        let is_sqlite = self.dialog.db_type.driver_id() == "sqlite";
        let engine_label = catalog_entry(&self.dialog.db_type)
            .map(|e| e.label.to_string())
            .unwrap_or_else(|| self.dialog.db_type.to_string());
        let port_hint = self
            .dialog
            .db_type
            .default_port()
            .map(|p| p.to_string())
            .unwrap_or_else(|| "port".into());
        let has_driver = self.dialog.db_type.is_builtin();

        let mut body = v_flex().p_4().gap_2();

        // Name
        body = body.child(
            h_flex()
                .items_center()
                .gap_2()
                .child(Self::dialog_label("Name"))
                .child(Input::new(&self.dialog.name).small().flex_1()),
        );

        // Status color + tag
        let swatch_list: Vec<_> = STATUS_COLORS
            .iter()
            .enumerate()
            .map(|(ix, swatch)| {
                let selected = self.dialog.color_ix == Some(ix);
                div()
                    .id(SharedString::from(format!("status-color-{ix}")))
                    .w(px(16.))
                    .h(px(16.))
                    .flex_shrink_0()
                    .rounded_full()
                    .bg(gpui::rgb(*swatch))
                    .border_2()
                    .border_color(if selected {
                        color(COLOR_TEXT)
                    } else {
                        gpui::hsla(0., 0., 0., 0.)
                    })
                    .hover(|this| this.border_color(color(COLOR_MUTED)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.dialog.color_ix = if this.dialog.color_ix == Some(ix) {
                            None
                        } else {
                            Some(ix)
                        };
                        cx.notify();
                    }))
            })
            .collect();
        body = body.child(
            h_flex()
                .items_center()
                .gap_2()
                .child(Self::dialog_label("Status color"))
                .child(h_flex().gap_1p5().children(swatch_list))
                .child(div().flex_1())
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(color(COLOR_MUTED))
                        .child("Tag"),
                )
                .child(Input::new(&self.dialog.tag).small().w(px(140.))),
        );

        // Host + port (TCP engines only)
        if !is_sqlite {
            body = body.child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(Self::dialog_label("Host"))
                    .child(Input::new(&self.dialog.host).small().flex_1())
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(color(COLOR_MUTED))
                            .child("Port"),
                    )
                    .child(Input::new(&self.dialog.port).small().w(px(96.)))
                    .child(
                        div()
                            .w(px(16.))
                            .text_size(px(10.5))
                            .text_color(color(COLOR_MUTED))
                            .child(port_hint.clone()),
                    ),
            );

            // User
            body = body.child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(Self::dialog_label("User"))
                    .child(Input::new(&self.dialog.user).small().flex_1()),
            );

            // Password + store in keychain
            body = body.child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(Self::dialog_label("Password"))
                    .child(
                        Input::new(&self.dialog.password)
                            .small()
                            .mask_toggle()
                            .flex_1(),
                    )
                    .child(
                        Checkbox::new("store-in-keychain")
                            .label("Store in keychain")
                            .checked(self.dialog.store_password)
                            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                this.dialog.store_password = *checked;
                                cx.notify();
                            })),
                    ),
            );

            // Database
            body = body.child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(Self::dialog_label("Database"))
                    .child(Input::new(&self.dialog.database).small().flex_1()),
            );

            // SSL mode
            body = body.child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(Self::dialog_label("SSL mode"))
                    .child(
                        Button::new("ssl-mode")
                            .small()
                            .outline()
                            .child(
                                h_flex()
                                    .gap_1p5()
                                    .child(
                                        div()
                                            .text_size(px(11.))
                                            .line_height(px(1.))
                                            .child(ssl_mode_label(self.dialog.ssl)),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(10.))
                                            .line_height(px(1.))
                                            .text_color(color(COLOR_MUTED))
                                            .child("▾"),
                                    ),
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.dialog.ssl = next_ssl_mode(this.dialog.ssl);
                                cx.notify();
                            })),
                    )
                    .child(div().flex_1()),
            );

            // SSL keys (certificate files land in a later milestone)
            let ssl_key_button = |id: &'static str, label: &'static str| {
                Button::new(id)
                    .small()
                    .outline()
                    .child(label)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.toast(
                            Notification::warning(
                                "SSL certificate files aren't supported yet",
                            )
                            .title("SSL keys"),
                            window,
                            cx,
                        );
                    }))
            };
            body = body.child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(Self::dialog_label("SSL keys"))
                    .child(
                        h_flex()
                            .gap_1p5()
                            .child(ssl_key_button("ssl-key", "Key…"))
                            .child(ssl_key_button("ssl-cert", "Cert…"))
                            .child(ssl_key_button("ssl-ca", "CA Cert…")),
                    )
                    .child(div().flex_1()),
            );
        } else {
            // SQLite: a file path (or :memory:) instead of TCP fields.
            body = body.child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(Self::dialog_label("File"))
                    .child(Input::new(&self.dialog.database).small().flex_1())
                    .child(
                        div()
                            .w(px(140.))
                            .text_size(px(10.5))
                            .text_color(color(COLOR_MUTED))
                            .child("path or :memory:"),
                    ),
            );
        }

        // Engines without a driver in this binary: say so up front.
        if !has_driver {
            body = body.child(
                div()
                    .text_size(px(11.))
                    .text_color(color(COLOR_AMBER))
                    .child(format!(
                        "No driver installed for {engine_label} yet — the connection can be saved, but Test/Connect will fail."
                    )),
            );
        }

        // Optional SSH tunnel section
        if self.dialog.over_ssh {
            body = body.child(
                v_flex()
                    .gap_2()
                    .pt_1()
                    .mt_1()
                    .border_t_1()
                    .border_color(color(COLOR_BORDER_SOFT))
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(color(COLOR_MUTED))
                            .child("OVER SSH"),
                    )
                    .child(
                        h_flex()
                            .items_center()
                            .gap_2()
                            .child(Self::dialog_label("SSH host"))
                            .child(Input::new(&self.dialog.ssh_host).small().flex_1())
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(color(COLOR_MUTED))
                                    .child("Port"),
                            )
                            .child(Input::new(&self.dialog.ssh_port).small().w(px(96.))),
                    )
                    .child(
                        h_flex()
                            .items_center()
                            .gap_2()
                            .child(Self::dialog_label("SSH user"))
                            .child(Input::new(&self.dialog.ssh_user).small().flex_1()),
                    )
                    .child(
                        h_flex()
                            .items_center()
                            .gap_2()
                            .child(Self::dialog_label("SSH password"))
                            .child(Input::new(&self.dialog.ssh_password).small().flex_1()),
                    )
                    .child(
                        div()
                            .text_size(px(10.5))
                            .text_color(color(COLOR_MUTED))
                            .child(
                                "The tunnel is stored with the connection; opening it lands in a later milestone.",
                            ),
                    ),
            );
        }

        v_flex()
            .id("conn-details-card")
            // Occlude the backdrop so form clicks don't dismiss the dialog.
            .occlude()
            .w(px(560.))
            .max_h(px(620.))
            .overflow_y_scroll()
            .rounded(px(10.))
            .border_1()
            .border_color(border)
            .bg(color(COLOR_SURFACE))
            .shadow_lg()
            // Header: red close dot + centered title, TablePlus-style.
            .child(
                h_flex()
                    .h(px(40.))
                    .flex_shrink_0()
                    .items_center()
                    .justify_center()
                    .border_b_1()
                    .border_color(border)
                    .relative()
                    .child(
                        div()
                            .id("dialog-close-dot")
                            .absolute()
                            .left(px(14.))
                            .top(px(14.))
                            .w(px(12.))
                            .h(px(12.))
                            .rounded_full()
                            .bg(color(COLOR_RED))
                            .hover(|this| this.opacity(0.8))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.close_conn_dialog(cx)
                            })),
                    )
                    .child(
                        div()
                            .text_size(px(13.))
                            .font_weight(FontWeight::MEDIUM)
                            .child(format!("{engine_label} Connection")),
                    ),
            )
            .child(body)
            .child(
                h_flex()
                    .flex_shrink_0()
                    .gap_2()
                    .px_4()
                    .py_3()
                    .border_t_1()
                    .border_color(border)
                    .bg(color(COLOR_SURFACE2))
                    .child(
                        Button::new("dialog-over-ssh")
                            .small()
                            .outline()
                            .when(self.dialog.over_ssh, |b| b.primary())
                            .child(
                                h_flex()
                                    .gap_1()
                                    .child(
                                        div().text_size(px(12.)).line_height(px(1.)).child("⇄"),
                                    )
                                    .child("Over SSH"),
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.dialog.over_ssh = !this.dialog.over_ssh;
                                cx.notify();
                            })),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("dialog-save")
                            .small()
                            .outline()
                            .child(
                                h_flex()
                                    .gap_1()
                                    .child(
                                        div().text_size(px(12.)).line_height(px(1.)).child("✓"),
                                    )
                                    .child("Save"),
                            )
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.dialog_save(window, cx)
                            })),
                    )
                    .child(
                        Button::new("dialog-test")
                            .small()
                            .outline()
                            .disabled(self.conn_testing)
                            .child(
                                h_flex()
                                    .gap_1()
                                    .child(
                                        div().text_size(px(12.)).line_height(px(1.)).child("⚡"),
                                    )
                                    .child(if self.conn_testing { "Testing…" } else { "Test" }),
                            )
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.dialog_test(window, cx)
                            })),
                    )
                    .child(
                        Button::new("dialog-connect")
                            .small()
                            .primary()
                            .disabled(self.running)
                            .child(
                                h_flex()
                                    .gap_1()
                                    .child(
                                        div().text_size(px(12.)).line_height(px(1.)).child("→"),
                                    )
                                    .child("Connect"),
                            )
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.dialog_connect(window, cx)
                            })),
                    ),
            )
    }

    /// The "Import from URL" screen: paste a connection string, ⏎ or the
    /// button parses it and continues to the details form.
    fn render_url_import(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let border = color(COLOR_BORDER);
        v_flex()
            .w(px(520.))
            .occlude()
            .rounded(px(10.))
            .border_1()
            .border_color(border)
            .bg(color(COLOR_SURFACE))
            .shadow_lg()
            .overflow_hidden()
            .child(
                h_flex()
                    .h(px(40.))
                    .flex_shrink_0()
                    .items_center()
                    .justify_center()
                    .border_b_1()
                    .border_color(border)
                    .relative()
                    .child(
                        div()
                            .id("url-close-dot")
                            .absolute()
                            .left(px(14.))
                            .top(px(14.))
                            .w(px(12.))
                            .h(px(12.))
                            .rounded_full()
                            .bg(color(COLOR_RED))
                            .hover(|this| this.opacity(0.8))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.close_conn_dialog(cx)
                            })),
                    )
                    .child(
                        div()
                            .text_size(px(13.))
                            .font_weight(FontWeight::MEDIUM)
                            .child("Import from URL"),
                    ),
            )
            .child(
                v_flex()
                    .p_4()
                    .gap_2()
                    .child(Input::new(&self.dialog.url).small())
                    .child(
                        div()
                            .text_size(px(10.5))
                            .text_color(color(COLOR_MUTED))
                            .child(
                                "The scheme picks the engine: postgresql, mysql, mariadb, sqlite, mssql, mongodb, redis, cassandra, snowflake, oracle, vertica, cockroach…",
                            ),
                    ),
            )
            .child(
                h_flex()
                    .justify_between()
                    .px_4()
                    .py_3()
                    .border_t_1()
                    .border_color(border)
                    .bg(color(COLOR_SURFACE2))
                    .child(
                        Button::new("url-back")
                            .small()
                            .ghost()
                            .child(
                                h_flex()
                                    .gap_1()
                                    .child(
                                        div().text_size(px(12.)).line_height(px(1.)).child("←"),
                                    )
                                    .child("Back"),
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.dialog.step = ConnDialogStep::PickType;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("url-apply")
                            .small()
                            .primary()
                            .child(
                                h_flex()
                                    .gap_1()
                                    .child(
                                        div().text_size(px(12.)).line_height(px(1.)).child("⇩"),
                                    )
                                    .child("Parse & Continue"),
                            )
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.dialog_apply_url(window, cx)
                            })),
                    ),
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
