//! SQL Pro — a native, cross-platform database client.
//!
//! A GPUI port of TablePro's architecture: one `DatabaseDriver` trait, a
//! driver per engine behind cargo features, and a data-grid-first UI.

mod grid;
mod store;
mod workspace;

use gpui_kit::prelude::*;
use gpui_kit::{application, init, open_window, WindowOptions};

use crate::workspace::Workspace;

fn main() {
    application().run(|cx| {
        init(cx);

        open_window(WindowOptions::default(), cx, |window, cx| {
            cx.new(|cx| Workspace::new(window, cx))
        })
        .expect("failed to open main window");

        cx.activate(true);
    });
}
