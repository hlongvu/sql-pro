//! SQL Pro — a native, cross-platform database client.
//!
//! A GPUI port of TablePro's architecture: one `DatabaseDriver` trait, a
//! driver per engine behind cargo features, and a data-grid-first UI.

// GPUI's derive/macros (and the `#[gpui::test]` harness) exceed the default
// macro-expansion depth.
#![recursion_limit = "1024"]

mod grid;
mod saved_queries;
mod secret_store;
mod storage;
mod store;
mod theme;
mod workspace;

#[cfg(test)]
mod conn_dialog_tests;
#[cfg(test)]
mod toast_tests;

// The prelude brings gpui's `AppContext`/element builders into scope. The
// bare `gpui_kit::*` glob must NOT be re-added here: it re-exports
// gpui-kit's `pub use crate as gpui` self-alias, which makes the
// `#[gpui::test]` macro's generated `gpui::…` paths resolve into an
// infinite loop.
use gpui_kit::prelude::*;
use gpui_kit::{application, init, open_window, WindowOptions};
use std::sync::Arc;

use crate::workspace::Workspace;

fn main() {
    application().run(|cx| {
        init(cx);

        // The interface spec calls for a compact, dark, desktop-first client:
        // near-black canvas, one-pixel separators, restrained blue accent.
        // The status-bar toggle can switch to the light palette at runtime.
        crate::theme::apply(true, cx);

        open_window(app_window_options(), cx, |window, cx| {
            cx.new(|cx| Workspace::new(window, cx))
        })
        .expect("failed to open main window");

        cx.activate(true);
    });
}

/// Native window metadata shared by every platform build.
///
/// GPUI currently applies `icon` to X11 windows; keeping the asset embedded
/// here also ensures it is compiled into every SQL Pro binary, ready for the
/// platform packaging step on macOS and Windows.
fn app_window_options() -> WindowOptions {
    let icon = image::load_from_memory(include_bytes!("../../../assets/sql-pro-icon-yellow.png"))
        .expect("SQL Pro app icon must be a valid PNG")
        .into_rgba8();

    WindowOptions {
        app_id: Some("com.sqlpro.app".into()),
        icon: Some(Arc::new(icon)),
        ..WindowOptions::default()
    }
}
