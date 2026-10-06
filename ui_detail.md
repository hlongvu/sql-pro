# TablePro UI/UX plan

## Purpose and design direction

TablePro is a native database browser. Its interface should prioritize the
three actions that users repeat all day:

1. Find a database object.
2. Browse, filter, and safely edit its rows.
3. Open and run SQL without losing their place in the data browser.

The visual reference is a compact, dark, desktop-first database client: a
single window with a low-height toolbar, dedicated connection and object
navigators, a query-plus-grid workspace, and an optional right-side row
inspector. The result must be an original TablePro interface, not a copy of
TablePlus branding, artwork, or proprietary assets.

## Layout map

```
┌────────────────────────────────── application toolbar ──────────────────────────────────┐
│ window controls | connection context | safety / reload / view controls                    │
├───────────────┬───────────────────┬────────────────────────────────┬──────────────────────┤
│ Connections   │ Tables & objects  │ Query editor                   │ Row inspector        │
│               │                   ├────────────────────────────────┤ (toggleable)         │
│ saved / live  │ selected          │ Table/grid cell view           │ Details | Assistant  │
│ connections   │ connection only   ├──────────────── grid footer ────┤ field search         │
│ + connection  │ schema + create   │ Data | Structure | Rows         │ editable field list  │
└───────────────┴───────────────────┴────────────────────────────────┴──────────────────────┘
```

## Base pane model

The primary workspace has **three base columns**. The row inspector is a
fourth, optional column that users can show or hide without changing the
three-column workflow.

| Pane | Default role | Required behavior |
| --- | --- | --- |
| 1. Connections | Lists saved and active connections. | Selecting a connection makes it the active context for pane 2 and pane 3. It must not replace or hide the connection list. |
| 2. Tables & objects | Lists the tables, views, functions, and saved queries belonging to the selected connection. | It refreshes when pane 1 changes. Selecting an object opens it in pane 3. |
| 3. Workspace | Holds the SQL query editor on top and the table-cell/result grid on the bottom. | The editor and grid remain vertically split for both object browsing and query work. The split is resizeable. |
| 4. Row inspector | Shows values and metadata for the selected row/cell. | Off by default when space is constrained; users can toggle it on/off from the toolbar or with a shortcut. Hiding it expands pane 3. |

Connection selection, object selection, and data selection are independent:

```
connection selected in pane 1
    → objects loaded in pane 2
        → object/query loaded in pane 3
            → selected row/cell optionally described in pane 4
```

### Sizing and responsiveness

- Toolbar: fixed 46–52 px height.
- Connections pane: default 210–240 px; resizeable; collapsible only at
  narrow widths.
- Tables & objects pane: default 240–280 px; resizeable; collapsible only at
  narrow widths.
- Row inspector: default 290–320 px; resizeable and toggleable.
- Central workspace: takes all remaining width; never permits its data grid
  to shrink below a usable column width.
- At narrow widths, hide the inspector first, then collapse the tables pane,
  and lastly reduce the connections pane to an icon rail. Preserve the central
  query/grid workspace.
- All four panes support independent scrolling. A navigator scroll must never move
  the data grid.

## Visual language

- Use a near-black/charcoal canvas with subtle one-pixel separators instead
  of card-heavy surfaces.
- Use compact desktop spacing: 6–10 px between controls, 10–14 px pane
  padding, and 30–36 px rows in navigators.
- Make the active connection unmistakable with a restrained status pill and a
  colored connection-state dot. Green means connected; amber means busy or
  caution; red means disconnected/error.
- The selected row gets the accent fill. Hover uses a low-contrast neutral
  fill. Staged row changes need their own distinct, accessible treatment:
  inserted (green tint), modified (amber tint), deleted (red tint plus dim).
- Use text labels for destructive or consequential actions (`Save`, `Revert`,
  `Delete row`) instead of icon-only controls.
- Avoid using the TablePlus elephant, logo, names, screenshots, or other
  branded visual assets.

## Application toolbar

The toolbar is the global context bar. It should be visually quiet and should
not contain controls that only apply to an individual table cell.

| Element | Behavior | States / notes |
| --- | --- | --- |
| Connections-pane toggle | Collapses or restores pane 1 when the user needs more workspace width. | Tooltip and shortcut shown on hover. |
| Objects-pane toggle | Collapses or restores pane 2 independently of the connections pane. | Tooltip and shortcut shown on hover. |
| Connection switcher | Opens saved connections and databases. | Shows engine icon, connection name, database/schema, and connection state. |
| `+ SQL` / SQL workspace | Opens a new query workspace without dropping the current table context. | `Cmd/Ctrl+E`. |
| Run | Runs the selected statement or statement under the caret. | `Cmd/Ctrl+Enter`; becomes a spinner and then a cancel action while running. |
| Save | Commits all staged insert, edit, and delete operations. | Disabled when no changes are staged; `Cmd/Ctrl+S`. |
| Revert | Discards every staged change in the current data workspace. | Requires a confirmation only when there are pending changes. |
| Export | Exports the visible result set or selected rows. | Opens format and destination choices; never silently exports full data when a filter is active. |
| Refresh | Reloads objects or re-runs the active browse query. | Warn if this would discard uncommitted edits. |
| Safe mode | Controls protections for risky write queries. | Off / warn / strict; clearly display the active mode. |
| Inspector toggle | Shows or hides the right row inspector. | `Space` when grid focus is active. |

### Connection context pill

The connection pill is the center-weighted anchor in the toolbar. It contains:

- Engine name and version when available.
- Connection nickname.
- Active database and schema/namespace.
- A state indicator: connected, connecting, read-only, warning, or offline.

Clicking it should open a contextual menu to switch database/schema, reconnect,
view connection properties, or copy connection metadata. It must never reveal
passwords, tokens, or SSH credentials.

## Pane 1: connections

The first column is always the connection list. It is the stable starting
point for navigation and is not mixed with database objects.

| Element | Behavior |
| --- | --- |
| Connection search | Fuzzy-searches saved connection names, host labels, and engine types. |
| Connection row | Shows engine icon, connection nickname, host/database summary, and a state dot. Single-click makes it active; double-click connects or reconnects. |
| Active connection | Uses the selected styling and becomes the sole source for the objects shown in pane 2. |
| `+ Connection` | Opens the connection-creation flow. |
| Connection context menu | Provides reconnect, duplicate, rename, organize, edit, disconnect, and remove actions. |

Recent connections can appear as a group at the top, but they remain
connections—not object history.

## Pane 2: tables and object navigator

The second column is an object finder for the connection selected in pane 1,
not a second data view. It starts empty until a connection is selected.

### Top controls

| Element | Behavior |
| --- | --- |
| Search field | Fuzzy-searches only currently loaded object names and types. Keyboard focus: `Cmd/Ctrl+Shift+F`. |
| Items tab | Shows database objects grouped by type. |
| Favorites tab | Shows user-pinned tables, views, saved queries, and folders. |
| History tab | Shows recent objects and executed queries scoped to the current connection. |
| Filter/settings icon | Lets users include/exclude object categories and choose whether recents are shown. |

### Object tree

Groups are collapsible and ordered consistently:

1. Recently opened objects (maximum five by default).
2. Tables.
3. Views.
4. Functions and procedures.
5. Saved queries/folders, when present.

Each object row has a type icon, its name, and optional non-sensitive metadata
(such as row count if already known). Single-click selects it; double-click or
`Enter` opens it. Context menus provide actions appropriate to its type:
open data, open structure, open in query, duplicate, rename, export, or
delete/truncate where supported.

### Object-pane footer

| Element | Behavior |
| --- | --- |
| Schema selector | Switches active namespace/schema and refreshes object groups. |
| `+ Table` | Opens a table-creation flow. |
| `+` menu | Creates an object available for the active database engine (table, view, saved query, or folder). |

## Pane 3: central query and data workspace

### Object workspace: data browser

The central workspace always has a query editor above the data/result view.
The editor is compact by default and vertically resizeable, so users can drag
it smaller when browsing rows or taller when authoring SQL. The grid header
and footer make the active object and data mode clear.

#### Data grid

| Element | Behavior |
| --- | --- |
| Row-number gutter | Identifies visual rows and selects an entire row. It does not imply a database primary key. |
| Column header | Displays name and type indicator; click to sort; context menu supports sort, hide, pin, copy name, and filter. |
| Resizer | Drag between headers to resize columns. Double-click fits a column to content with a safe width limit. |
| Cells | Single-click selects; `Enter`/double-click starts editing where allowed; `Esc` cancels cell editing. |
| Null display | Uses a muted `NULL` label rather than an empty-looking cell. |
| Binary/large value | Shows a compact placeholder and opens a value viewer on request. |
| Grid scrollbars | Both dimensions scroll independently; rows and columns must be virtualized. |

Grid interactions must preserve the source value, stage the proposed value,
and make the change visible before any database write occurs.

#### Grid action strip

| Element | Behavior |
| --- | --- |
| Data tab | Active by default; displays rows. |
| Structure tab | Shows columns, keys, indexes, constraints, and generated SQL. |
| `+ Row` | Stages a blank inserted row. Default-value fields remain database defaults until saved. |
| `− Row` | Stages deletion of selected rows. Must not execute immediately. |
| Columns | Opens a column chooser with show/hide and order controls. |
| Filters | Opens a filter builder. Each condition includes column, operator, value, and enabled state. |
| Paging controls | Navigate server-backed result pages where applicable. |
| Selection summary | Shows count of selected rows and count of staged changes. |

### Query workspace

A query workspace is separate from object browsing but keeps its connection
context. It has a small tab strip, an SQL editor, and results below or in
split-result tabs.

| Element | Behavior |
| --- | --- |
| Query tab | Names unsaved queries `Query 1`, `Query 2`, etc.; shows a dirty marker and close affordance. |
| SQL editor | Syntax-highlighted SQL, line numbers, autocomplete, selection execution, multiple statements, and formatting. |
| Run current | Executes selection or statement under caret. `Cmd/Ctrl+Enter`. |
| Run all | Executes all statements in editor. `Cmd/Ctrl+Shift+Enter`. |
| Cancel | Cancels the active query where the driver supports cancellation. |
| Result status | Shows duration, returned row count, affected rows, and errors without stealing editor focus. |
| Result tabs | One tab per result set by default; supports append mode as a preference. |

## Pane 4: optional row inspector

The optional right pane makes a selected row understandable and editable
without opening a modal. It can be toggled on/off from the toolbar and is
automatically hidden first at constrained widths.

### Inspector controls

| Element | Behavior |
| --- | --- |
| Details tab | Shows values for the selected row. |
| Assistant tab | Reserved for a future, opt-in helper. It must not execute SQL or transmit schema/data without explicit confirmation. |
| Field search | Filters visible field cards by column name. |
| Field label | Shows column name and metadata: type, primary key, nullable, generated/read-only. |
| Field value | Selectable and editable when supported. Edits stage a change; they do not write immediately. |
| Value affordance | Opens a larger editor/viewer for long text, JSON, bytes, images, or date/time values. |

When no row is selected, the inspector should say `No row selected` and retain
its controls. When multiple rows are selected, it should show a bulk-edit
summary and only fields safe for bulk changes.

## Status and feedback

The bottom status bar communicates non-blocking outcomes:

- Connection status and active engine.
- Query duration and row/affected-row count.
- Number of staged changes.
- Shortcut reminders appropriate to the focused workspace.
- Clear, copyable error messages with an optional `Details` disclosure.

Use toasts only for short-lived confirmations. A failed query, failed save, or
data-loss risk must remain visible until the user dismisses or resolves it.

## State model and safety rules

### Editing lifecycle

```
select cell/row → propose edit → stage change → review visual state
→ Save transaction → reload server truth
                         └→ Revert clears staged changes
```

- The UI must distinguish staged changes from committed database state at all
  times.
- `Save` sends parameterized writes inside a transaction when the engine
  supports it.
- A write affecting an unexpected number of rows is an error, not a silent
  success.
- Reload, connection changes, tab close, and schema changes warn before they
  discard staged work.
- Read-only connections disable destructive controls with an explanation.

### Empty, loading, and error states

| Situation | Required response |
| --- | --- |
| Connecting | Preserve existing workspace, show a connection-progress state, and disable incompatible actions. |
| Empty schema | Explain that no compatible objects were found and offer `+ Table` or a query workspace. |
| Empty result | Keep headers and filters visible; show `0 rows` rather than a blank grid. |
| Long query | Show elapsed time and Cancel without blocking navigation. |
| Query error | Highlight the error, preserve SQL text and result context, and offer copy details. |
| Save error | Keep staged changes intact and identify the failing statement/row when possible. |

## Accessibility and keyboard behavior

- Every icon-only button needs a visible tooltip and accessible name.
- Maintain keyboard focus rings with sufficient contrast.
- All navigator, grid, query, and inspector actions need keyboard paths.
- Do not rely on color alone to convey staged-change state; pair it with an
  icon, label, or accessible description.
- Preserve platform shortcuts: `Cmd` on macOS and `Ctrl` on Windows/Linux.

## Delivery order (no implementation in this plan)

1. Establish pane geometry, toolbar hierarchy, dark theme tokens, and
   responsive collapse rules.
2. Build the dedicated connections pane and the connection-scoped objects pane
   with search, object grouping, selection, and schema footer.
3. Complete the vertically split query-and-grid workspace: header interactions, grid footer,
   filters, columns, selection, and staged-row visual states.
4. Add the permanent details inspector and connect it to selected grid rows.
5. Refine the query workspace, result tabs, shortcuts, and cancellation.
6. Add safety states, empty/error/loading states, accessibility review, and
   visual regression screenshots across macOS, Linux, and Windows.
