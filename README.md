# SQL Pro

A fast, native, cross-platform database client built with **Rust + GPUI** — a port of [TablePro](https://github.com/TableProApp/TablePro)'s architecture from Swift/AppKit to a single Rust codebase that builds on macOS, Linux, and Windows.

```
cargo run -p sqlpro
```

## Why this exists

TablePro proves a native multi-database client can be fast and open source (AGPL). But it is Swift-only: macOS and iOS today, Linux a prototype, Windows nothing. This project ports its architecture to GPUI (Zed's GPU-accelerated UI framework) so one Rust codebase ships to all three desktops.

## Architecture

The port mirrors TablePro's design, crate by crate:

| TablePro (Swift)         | SQL Pro (Rust)                          |
| ------------------------ | --------------------------------------- |
| `TableProCoreTypes` — `DatabaseType`, `QueryResult` | `crates/core` — `DatabaseType` (open, string-backed), `Cell`, `QueryResult`, `ConnectionConfig` |
| `PluginDatabaseDriver` protocol + PluginManager | `sqlpro_core::driver::DatabaseDriver` trait + factory in `sqlpro_drivers` (cargo features replace plugin bundles for built-ins) |
| libpq / libmariadb / libsqlite3 / hiredis C statics | `tokio-postgres`, `mysql_async`, `rusqlite` (bundled) — pure Rust drivers |
| AppKit + SwiftUI views, `MainSplitViewController` | `crates/app` — GPUI workspace: sidebar, SQL editor, virtualized results grid |
| CodeEdit-fork editor + tree-sitter grammars | gpui-component `Editor` with tree-sitter SQL highlighting |
| CoreText cell renderer in `DataGridCoordinator` | gpui-component `DataTable` (same virtualization model: fixed rows, recycled cells, delegate-driven) |
| blocking C drivers + `CancellableBlockingWork` | async drivers on a shared Tokio runtime bridged to GPUI's executor (`drivers/src/runtime.rs`) |

```
sql-pro/
├── crates/
│   ├── core/       # Domain model + DatabaseDriver trait (no IO)
│   ├── drivers/    # Engine implementations behind cargo features
│   │   ├── sqlite.rs      # rusqlite, on the blocking pool
│   │   ├── postgres.rs    # tokio-postgres (Redshift/CockroachDB compatible)
│   │   ├── mysql.rs       # mysql_async (MariaDB/TiDB compatible)
│   │   ├── sql_split.rs   # multi-statement splitter (strings/comments/$tag$)
│   │   └── runtime.rs     # Tokio <-> GPUI executor bridge
│   └── app/        # GPUI desktop app (sidebar + editor + data grid)
└── Cargo.toml      # workspace
```

### Features

- **Multi-engine**: SQLite, PostgreSQL, MySQL out of the box (engine features can be toggled per build).
- **SQL editor** with tree-sitter syntax highlighting, multi-statement execution, ⌘⏎ / Ctrl+⏎ to run.
- **Editable results grid**: stage cell edits, insert/delete rows, then **Save** — the ported change-tracking pipeline generates parameterized `INSERT`/`UPDATE`/`DELETE` with keyed (PK) or keyless matching, sequence-ordered statements, affected-row verification, and transaction wrapping (TablePro's `PendingChanges` → `SQLStatementGenerator` → `DataWriteExecutor`). **Revert** restores server truth.
- **Export**: current result set to CSV (RFC-4180 quoting, formula sanitization, NULL handling).
- **Query history**: every query recorded to SQLite (`~/.local/share/sqlpro/query_history.db`) with FTS5 search, fingerprint grouping, and retention (10,000 entries / 90 days); searchable panel in the sidebar.
- **Connection sidebar**: saved connections (JSON at `~/.local/share/sqlpro/connections.json`), table browser with PK-aware click-to-browse, history panel.
- **Demo database**: ships with an in-memory SQLite instance so the app works with zero setup.
- **New connection form** for PostgreSQL in the sidebar footer.

### Tests

The TablePro test suite is being ported suite-by-suite with original test names (see [docs/test-port-inventory.md](docs/test-port-inventory.md) for the full inventory and progress — ~24,500 portable cases). Currently passing: **189 tests** covering change tracking, SQL statement generation (composite PK, no-PK, parameter styles, MSSQL quoting, ordering, batch caps), the write executor's verification semantics, CSV field/parser/inference, query history + fingerprints, and driver end-to-end.

### Roadmap (mirrors TablePro feature-by-feature)

- [ ] Inline cell editing → change tracking → SQL statement generation (TablePro's `DataChangeManager`/`SQLStatementGenerator`)
- [ ] SSH tunnels (russh), TLS modes, AWS RDS IAM / Entra / Google sign-in
- [ ] More engines as feature modules: ClickHouse (HTTP), Redis, SQL Server, MongoDB, DuckDB
- [ ] Schema-aware autocomplete (`SQLContextAnalyzer` port), query history (SQLite + FTS5)
- [ ] Export/import (CSV/JSON/SQL first), server dashboard, ER diagrams
- [ ] Theme engine with JSON theme slots, keymap customization
- [ ] Plugin system for third-party drivers (dynamic loading or WASM)

## Platform builds

| Platform | Status | Notes |
| -------- | ------ | ------ |
| macOS 12+ | ✅ builds & runs | tested on Apple Silicon |
| Linux | ✅ builds | needs `libwebkit2gtk-4.1-dev build-essential curl wget file libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev` (GPUI's X11/Wayland stack: `cmake`, `pkg-config`, `libfontconfig-dev`) |
| Windows 10+ | ✅ builds | MSVC toolchain; everything is vendored, no vcpkg needed |

GPUI (via `gpui-kit`) ships GPU rendering on all three platforms: Metal on macOS, Vulkan on Linux (blade), DirectX 12 on Windows.

### Build commands

```bash
# All platforms
cargo build --release -p sqlpro

# Smaller build without engines you don't need
cargo build -p sqlpro --no-default-features
cargo build -p sqlpro --features sqlpro-drivers/sqlite
```

## Development

```bash
cargo test              # unit tests (SQL splitter, core models)
cargo run -p sqlpro     # launch the app
```

## License

AGPL-3.0, same as TablePro, from which this port's architecture derives.
