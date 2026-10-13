//! Headless verification of the new-connection dialog: picking an engine,
//! importing a connection URL, and building a config from the form inputs.
//!
//! Declared from the crate root (sibling of `workspace`, not a child) for
//! the same `#[gpui::test]` self-alias reason as `toast_tests`.

use gpui::{
    AnyWindowHandle, AppContext, Entity, MouseButton, MouseDownEvent, MouseUpEvent, Pixels, Point,
    PlatformInput, TestAppContext, WindowHandle,
};
use gpui_kit::component::input::InputState;

use crate::workspace::{ConnDialog, ConnDialogStep, Workspace};

type RootHandle = WindowHandle<gpui_kit::base::Root>;

/// Dispatch a real left click at `position` through the window's event
/// pipeline (updates the mouse position, rebuilds the hit-test, runs the
/// down/up listeners).
fn simulate_click(window: &mut gpui::Window, cx: &mut gpui::App, position: Point<Pixels>) {
    window
        .dispatch_event(
            PlatformInput::MouseDown(MouseDownEvent {
                button: MouseButton::Left,
                position,
                modifiers: Default::default(),
                click_count: 1,
                first_mouse: false,
            }),
            cx,
        );
    window
        .dispatch_event(
            PlatformInput::MouseUp(MouseUpEvent {
                button: MouseButton::Left,
                position,
                modifiers: Default::default(),
                click_count: 1,
            }),
            cx,
        );
}

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

/// Type into one of the dialog's inputs the way a user would.
fn set_input(
    handle: &RootHandle,
    cx: &mut TestAppContext,
    ws: &Entity<Workspace>,
    pick: impl Fn(&ConnDialog) -> &Entity<InputState> + Copy + 'static,
    text: &str,
) {
    let input = AnyWindowHandle::from(*handle)
        .update(cx, |_, _, cx| ws.read(cx).dialog_input(pick).clone())
        .expect("workspace alive");
    AnyWindowHandle::from(*handle)
        .update(cx, |_, window, cx| {
            input.update(cx, |state, cx| state.set_value(text, window, cx));
        })
        .unwrap();
}

#[gpui::test]
fn picked_engine_and_form_inputs_build_a_config(cx: &mut TestAppContext) {
    let (handle, ws) = open_workspace(cx);
    AnyWindowHandle::from(*handle)
        .update(cx, |_, window, cx| {
            ws.update(cx, |ws, cx| {
                ws.open_conn_dialog(window, cx);
                ws.dialog_pick_engine(0, cx); // PostgreSQL
            });
        })
        .unwrap();

    set_input(&handle, cx, &ws, |d| &d.name, "dental");
    set_input(&handle, cx, &ws, |d| &d.host, "127.0.0.1");
    set_input(&handle, cx, &ws, |d| &d.port, "5433");
    set_input(&handle, cx, &ws, |d| &d.user, "postgres");
    set_input(&handle, cx, &ws, |d| &d.password, "secret");
    set_input(&handle, cx, &ws, |d| &d.database, "dental_hub");

    let config = AnyWindowHandle::from(*handle)
        .update(cx, |_, _, cx| {
            ws.read(cx).dialog_build_config(cx).expect("config builds")
        })
        .unwrap();
    assert_eq!(config.name, "dental");
    assert_eq!(config.database_type.id(), "postgresql");
    assert_eq!(config.host.as_deref(), Some("127.0.0.1"));
    assert_eq!(config.port, Some(5433));
    assert_eq!(config.username.as_deref(), Some("postgres"));
    assert_eq!(config.password.as_deref(), Some("secret"));
    assert_eq!(config.database.as_deref(), Some("dental_hub"));
    assert_eq!(config.port(), Some(5433), "explicit port wins over default");
}

#[gpui::test]
fn importing_a_url_fills_the_details_step(cx: &mut TestAppContext) {
    let (handle, ws) = open_workspace(cx);
    AnyWindowHandle::from(*handle)
        .update(cx, |_, window, cx| {
            ws.update(cx, |ws, cx| {
                ws.open_conn_dialog(window, cx);
            });
        })
        .unwrap();

    set_input(
        &handle,
        cx,
        &ws,
        |d| &d.url,
        "postgres://bob:pw@db.local:5434/sales?sslmode=require",
    );
    AnyWindowHandle::from(*handle)
        .update(cx, |_, window, cx| {
            ws.update(cx, |ws, cx| ws.dialog_apply_url(window, cx));
        })
        .unwrap();

    AnyWindowHandle::from(*handle)
        .update(cx, |_, _, cx| {
            let dialog = &ws.read(cx).dialog;
            assert_eq!(dialog.step, ConnDialogStep::Details, "import advances to the form");
            assert_eq!(dialog.db_type.id(), "postgresql");
            assert_eq!(dialog.engine_ix, Some(0), "catalog engine is highlighted");
            assert_eq!(dialog.host.read(cx).value(), "db.local", "host prefilled");
            assert_eq!(dialog.port.read(cx).value(), "5434");
            assert_eq!(dialog.user.read(cx).value(), "bob");
            assert_eq!(dialog.password.read(cx).value(), "pw");
            assert_eq!(dialog.database.read(cx).value(), "sales");
            assert_eq!(dialog.ssl, sqlpro_core::connection::SslMode::Require);
        })
        .unwrap();

    let config = AnyWindowHandle::from(*handle)
        .update(cx, |_, _, cx| {
            ws.read(cx).dialog_build_config(cx).expect("imported form builds")
        })
        .unwrap();
    assert_eq!(config.host.as_deref(), Some("db.local"));
    assert_eq!(config.port, Some(5434));
    assert_eq!(config.ssl_mode, sqlpro_core::connection::SslMode::Require);
    assert!(
        config.name.contains("db.local"),
        "auto name mentions the host, got {:?}",
        config.name
    );
}

#[gpui::test]
fn sqlite_selection_builds_a_path_config(cx: &mut TestAppContext) {
    let (handle, ws) = open_workspace(cx);
    AnyWindowHandle::from(*handle)
        .update(cx, |_, window, cx| {
            ws.update(cx, |ws, cx| {
                ws.open_conn_dialog(window, cx);
                // SQLite sits at index 8 of the picker catalog.
                ws.dialog_pick_engine(8, cx);
            });
        })
        .unwrap();
    set_input(&handle, cx, &ws, |d| &d.database, ":memory:");

    let config = AnyWindowHandle::from(*handle)
        .update(cx, |_, _, cx| {
            ws.read(cx)
                .dialog_build_config(cx)
                .expect("sqlite config builds")
        })
        .unwrap();
    assert_eq!(config.database_type.id(), "sqlite");
    assert_eq!(config.path.as_deref(), Some(":memory:"));
    assert_eq!(config.host, None, "sqlite needs no host");
    assert!(
        config.name.starts_with("SQLite"),
        "auto name, got {:?}",
        config.name
    );
}

#[gpui::test]
fn missing_host_is_a_build_error_not_a_panic(cx: &mut TestAppContext) {
    let (handle, ws) = open_workspace(cx);
    AnyWindowHandle::from(*handle)
        .update(cx, |_, window, cx| {
            ws.update(cx, |ws, cx| {
                ws.open_conn_dialog(window, cx);
                ws.dialog_pick_engine(0, cx);
            });
        })
        .unwrap();
    AnyWindowHandle::from(*handle)
        .update(cx, |_, _, cx| {
            let err = ws
                .read(cx)
                .dialog_build_config(cx)
                .expect_err("empty host must fail validation");
            assert!(
                err.contains("Host"),
                "error should name the missing field, got {err}"
            );
        })
        .unwrap();
}

#[gpui::test]
fn every_dialog_step_draws(cx: &mut TestAppContext) {
    let (handle, ws) = open_workspace(cx);
    AnyWindowHandle::from(*handle)
        .update(cx, |_, window, cx| {
            ws.update(cx, |ws, cx| {
                ws.open_conn_dialog(window, cx);
            });
            // Step 1: the engine picker over the welcome screen.
            window.draw(cx);

            ws.update(cx, |ws, cx| {
                ws.dialog_pick_engine(0, cx);
                ws.dialog_advance(cx);
            });
            // Step 2: the details form (PostgreSQL has every field + SSH).
            window.draw(cx);

            ws.update(cx, |ws, cx| {
                ws.dialog.over_ssh = true;
            });
            window.draw(cx);

            ws.update(cx, |ws, cx| {
                ws.dialog.step = ConnDialogStep::ImportUrl;
            });
            // Step 3: the URL import screen.
            window.draw(cx);

            ws.update(cx, |ws, cx| {
                ws.dialog.db_type = sqlpro_core::database_type::DatabaseType::new(
                    sqlpro_core::database_type::DatabaseType::SQLITE,
                );
                ws.dialog.engine_ix = Some(8);
                ws.dialog.step = ConnDialogStep::Details;
            });
            // SQLite variant of the form (file path, no SSL rows).
            window.draw(cx);
        })
        .unwrap();
}

#[gpui::test]
fn clicking_inside_the_dialog_card_does_not_dismiss_it(cx: &mut TestAppContext) {
    let (handle, ws) = open_workspace(cx);
    AnyWindowHandle::from(*handle)
        .update(cx, |_, window, cx| {
            ws.update(cx, |ws, cx| {
                ws.open_conn_dialog(window, cx);
            });
            window.draw(cx);

            // The card is centered, so the viewport center lands on it.
            let size = window.viewport_size();
            simulate_click(
                window,
                cx,
                Point::new(size.width / 2.0, size.height / 2.0),
            );
            window.draw(cx);

            let ws = ws.read(cx);
            assert!(
                ws.dialog_open,
                "a click inside the card must never reach the close-on-click backdrop"
            );
            assert_eq!(ws.dialog.step, ConnDialogStep::PickType);
        })
        .unwrap();
}

#[gpui::test]
fn clicking_outside_the_dialog_card_dismisses_it(cx: &mut TestAppContext) {
    let (handle, ws) = open_workspace(cx);
    AnyWindowHandle::from(*handle)
        .update(cx, |_, window, cx| {
            ws.update(cx, |ws, cx| {
                ws.open_conn_dialog(window, cx);
            });
            window.draw(cx);

            // Top-left corner: outside the centered card, on the backdrop.
            simulate_click(window, cx, Point::new(px(8.), px(8.)));
            window.draw(cx);

            assert!(
                !ws.read(cx).dialog_open,
                "clicking the dimmed backdrop must dismiss the dialog"
            );
        })
        .unwrap();
}

fn px(value: f32) -> Pixels {
    Pixels::from(value)
}
