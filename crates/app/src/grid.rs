//! The results data grid.
//!
//! TablePro draws cells with CoreText into an NSTableView's rects with a
//! CTLine cache; GPUI's DataTable already virtualizes rows and columns the
//! same way, so the delegate here only supplies data and cell rendering,
//! plus the row visual states (modified/inserted/deleted) that TablePro's
//! `RowVisualState` carries.
//!
//! Inline cell editing: the delegate renders the workspace's cell editor
//! input directly inside the cell being edited (`editing`), so edits happen
//! in place on the table. The workspace owns the editor entity and commits
//! its value on Enter/blur.

use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::table::{Column, TableDelegate, TableState};
use gpui_kit::component::{ActiveTheme, Sizable};
use gpui_kit::*;
use sqlpro_core::value::CellValue;

/// Visual state of a row in the grid (TablePro's `RowVisualState` subset).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RowVisual {
    #[default]
    Normal,
    Modified,
    Inserted,
    Deleted,
}

pub struct ResultsDelegate {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<CellValue>>,
    pub visual: Vec<RowVisual>,
    /// The cell currently being edited inline, if any.
    pub editing: Option<(usize, usize)>,
    /// The shared cell-editor input, rendered inside the editing cell.
    pub editor: Option<Entity<InputState>>,
}

impl ResultsDelegate {
    pub fn empty() -> Self {
        Self {
            columns: Vec::new(),
            rows: Vec::new(),
            visual: Vec::new(),
            editing: None,
            editor: None,
        }
    }
}

impl TableDelegate for ResultsDelegate {
    fn columns_count(&self, _cx: &App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, _cx: &App) -> usize {
        self.rows.len()
    }

    fn column(&self, col_ix: usize, _cx: &App) -> Column {
        let name = self.columns.get(col_ix).cloned().unwrap_or_default();
        Column::new(format!("column-{col_ix}"), name)
            .width(px(180.))
            .resizable(true)
    }

    fn render_tr(
        &mut self,
        row_ix: usize,
        _window: &mut Window,
        _cx: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        let visual = self.visual.get(row_ix).copied().unwrap_or_default();
        let mut row = div().id(("row", row_ix));
        match visual {
            RowVisual::Modified => row = row.bg(gpui::hsla(50.0 / 360.0, 0.7, 0.5, 0.15)),
            RowVisual::Inserted => row = row.bg(gpui::hsla(120.0 / 360.0, 0.6, 0.5, 0.15)),
            RowVisual::Deleted => row = row.bg(gpui::hsla(0.0, 0.7, 0.5, 0.15)).opacity(0.6),
            RowVisual::Normal => {}
        }
        row
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        // The cell being edited renders the editor input in place.
        if self.editing == Some((row_ix, col_ix)) {
            if let Some(editor) = self.editor.clone() {
                return div()
                    .w_full()
                    .h_full()
                    .flex()
                    .items_center()
                    .child(Input::new(&editor).small())
                    .into_any_element();
            }
        }

        let Some(row) = self.rows.get(row_ix) else {
            return div().into_any_element();
        };
        let Some(cell) = row.get(col_ix) else {
            return div().into_any_element();
        };
        match cell {
            CellValue::Null => div()
                .text_color(cx.theme().muted_foreground)
                .italic()
                .child("NULL")
                .into_any_element(),
            CellValue::Bytes(_) => div()
                .text_color(cx.theme().muted_foreground)
                .child("<binary>")
                .into_any_element(),
            CellValue::Text(_) => div().child(cell.display()).into_any_element(),
        }
    }
}
