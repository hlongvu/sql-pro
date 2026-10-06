//! SQL Pro — a native, cross-platform database client.
//!
//! A GPUI port of TablePro's architecture: one `DatabaseDriver` trait, a
//! driver per engine behind cargo features, and a data-grid-first UI.

mod grid;
mod store;
mod workspace;

use gpui_kit::component::theme::{Theme, ThemeMode};
use gpui_kit::prelude::*;
use gpui_kit::*;
use gpui_kit::{application, init, open_window, WindowOptions};

use crate::workspace::Workspace;

fn main() {
    application().run(|cx| {
        init(cx);

        // The interface spec calls for a compact, dark, desktop-first client:
        // near-black canvas, one-pixel separators, restrained blue accent.
        Theme::change(ThemeMode::Dark, None, cx);
        Theme::update(cx, |theme| {
            let accent = rgb(0x2F7CF6).into();
            let accent_hover = rgb(0x2A6FDE).into();
            theme.accent = accent;
            theme.button_primary = accent;
            theme.button_primary_hover = accent_hover;
            theme.button_primary_active = accent;

            // Charcoal canvas and surfaces so the chrome reads as one
            // continuous workspace rather than stacked cards.
            theme.background = rgb(0x1B1D21).into();
            theme.foreground = rgb(0xE7E9EC).into();
            theme.border = rgb(0x33363C).into();
            theme.title_bar = rgb(0x25272C).into();
            theme.title_bar_border = rgb(0x33363C).into();
            theme.status_bar = rgb(0x202227).into();
            theme.status_bar_border = rgb(0x33363C).into();
            theme.tab_bar = rgb(0x25272C).into();
            theme.secondary = rgb(0x2B2E34).into();
            theme.secondary_hover = rgb(0x34383F).into();
            theme.muted = rgb(0x2B2E34).into();
            theme.muted_foreground = rgb(0x8B909A).into();
            theme.sidebar = rgb(0x202227).into();
            theme.sidebar_border = rgb(0x33363C).into();
            theme.input = rgb(0x14161A).into();
            theme.popover = rgb(0x25272C).into();
            theme.table = rgb(0x1B1D21).into();
            theme.table_head = rgb(0x25272C).into();
            theme.table_even = rgb(0x1E2025).into();
            theme.table_hover = rgb(0x2B2E34).into();
            theme.table_row_border = rgb(0x26292E).into();
            theme.selection = rgb(0x2F7CF6).into();
        });

        open_window(WindowOptions::default(), cx, |window, cx| {
            cx.new(|cx| Workspace::new(window, cx))
        })
        .expect("failed to open main window");

        cx.activate(true);
    });
}
