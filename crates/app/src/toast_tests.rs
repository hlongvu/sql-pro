//! Headless verification of the toast (Notification) pipeline.
//!
//! Declared from the crate root — deliberately NOT a child of
//! `workspace`, because the ancestor modules glob-import `gpui_kit::*`,
//! and gpui-kit re-exports itself as `gpui` (`pub use crate as gpui`).
//! That self-alias shadows the real `gpui` extern crate that the
//! `#[gpui::test]` macro's generated `gpui::…` paths must resolve to, and
//! the resolver loops forever.

use gpui::{AnyWindowHandle, AppContext, Entity, TestAppContext, WindowHandle};
use gpui_kit::component::WindowExt;

use crate::workspace::Workspace;

type RootHandle = WindowHandle<gpui_kit::base::Root>;

/// Open the workspace wrapped in gpui-kit's base Root, mirroring
/// `gpui_kit::open_window` (init + theme + Root plugin host).
fn open_workspace(cx: &mut TestAppContext) -> (RootHandle, Entity<Workspace>) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        crate::theme::apply(true, cx);
    });
    let mut ws_cell: Option<Entity<Workspace>> = None;
    let handle = cx.add_window(|window, cx| {
        let ws = cx.new(|cx| Workspace::new(window, cx));
        ws_cell = Some(ws.clone());
        gpui_kit::base::Root::new(ws, window, cx)
    });
    let ws = ws_cell.expect("workspace entity captured");
    (handle, ws)
}

fn toast_count(cx: &mut TestAppContext, handle: &RootHandle) -> usize {
    AnyWindowHandle::from(*handle)
        .update(cx, |_, window, cx| window.notifications(cx).len())
        .expect("window still open")
}

#[gpui::test]
fn save_with_nothing_staged_pushes_a_toast(cx: &mut TestAppContext) {
    let (handle, ws) = open_workspace(cx);
    AnyWindowHandle::from(handle)
        .update(cx, |_, window, cx| {
            ws.update(cx, |ws, cx| ws.save_changes(window, cx));
        })
        .unwrap();
    assert_eq!(
        toast_count(cx, &handle),
        1,
        "⌘S with nothing staged must surface a visible toast, not only an 11px footer line"
    );
}

#[gpui::test]
fn delete_row_without_selection_pushes_a_toast(cx: &mut TestAppContext) {
    let (handle, ws) = open_workspace(cx);
    AnyWindowHandle::from(handle)
        .update(cx, |_, window, cx| {
            ws.update(cx, |ws, cx| ws.delete_row(window, cx));
        })
        .unwrap();
    assert_eq!(
        toast_count(cx, &handle),
        1,
        "delete-row with no selection must surface a visible toast"
    );
}
