//! Light/dark theming.
//!
//! `apply` sets the gpui-component theme tokens for the requested mode.
//! The workspace chrome reads its palette through `workspace::color`, which
//! consults `is_dark()` so a single static flips both the components and
//! the hand-drawn panes at once.

use gpui_kit::component::theme::{Theme, ThemeMode};
use gpui_kit::prelude::*;
use gpui_kit::*;

use std::sync::atomic::{AtomicBool, Ordering};

static DARK: AtomicBool = AtomicBool::new(true);

pub fn is_dark() -> bool {
    DARK.load(Ordering::Relaxed)
}

/// Apply the full theme (components + chrome palette flag) for the mode.
pub fn apply(dark: bool, cx: &mut App) {
    DARK.store(dark, Ordering::Relaxed);

    let accent = rgb(0x2F7CF6).into();
    let accent_hover = rgb(0x2A6FDE).into();

    if dark {
        Theme::change(ThemeMode::Dark, None, cx);
        Theme::update(cx, |theme| {
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
            theme.selection = accent;
        });
    } else {
        Theme::change(ThemeMode::Light, None, cx);
        Theme::update(cx, |theme| {
            theme.accent = accent;
            theme.button_primary = accent;
            theme.button_primary_hover = accent_hover;
            theme.button_primary_active = accent;

            // The original light palette: quiet gray chrome on white.
            theme.background = rgb(0xFFFFFF).into();
            theme.foreground = rgb(0x1D1D1F).into();
            theme.border = rgb(0xE2E2E5).into();
            theme.title_bar = rgb(0xF7F7F8).into();
            theme.title_bar_border = rgb(0xE2E2E5).into();
            theme.status_bar = rgb(0xF5F5F6).into();
            theme.status_bar_border = rgb(0xE2E2E5).into();
            theme.tab_bar = rgb(0xF7F7F8).into();
            theme.secondary = rgb(0xEFEFF1).into();
            theme.secondary_hover = rgb(0xE2E2E5).into();
            theme.muted = rgb(0xF0F0F2).into();
            theme.muted_foreground = rgb(0x86868B).into();
            theme.sidebar = rgb(0xF5F5F6).into();
            theme.sidebar_border = rgb(0xE2E2E5).into();
            theme.input = rgb(0xFFFFFF).into();
            theme.popover = rgb(0xFFFFFF).into();
            theme.table = rgb(0xFFFFFF).into();
            theme.table_head = rgb(0xF0F0F2).into();
            theme.table_even = rgb(0xFAFAFB).into();
            theme.table_hover = rgb(0xE8E8EA).into();
            theme.table_row_border = rgb(0xECECEE).into();
            theme.selection = accent;
        });
    }
}
