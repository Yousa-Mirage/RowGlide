//! Headless regression tests dispatch production key bindings through a real window.

use super::{Workspace, state::TabMode};
use gpui_kit::test::TestWindowExt;
use gpui_kit::{AppContext, Entity, Focusable, TestAppContext, WindowOptions};
use rowglide_core::{Backend, CacheLimits, Cancellation, MockBackend, Preview, Query, ResultStore};
use std::sync::Arc;

/// Open a production workspace with deterministic retained mock rows and no pending query.
fn fixture(
    cx: &mut TestAppContext,
) -> Result<(gpui_kit::AnyWindowHandle, Entity<Workspace>), Box<dyn std::error::Error>> {
    let store = ResultStore::new(CacheLimits::default())?;
    let cancel = Cancellation::default();
    let preview = Preview::collect(
        MockBackend.query(Query::default(), cancel.clone())?,
        &cancel,
    )?;
    cx.update(|cx| {
        gpui_kit::init(cx);
        super::init(cx);
        gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
            cx.new(|cx| {
                let mut view = Workspace::new(Arc::new(MockBackend), store, Vec::new(), window, cx);
                view.cancellation.cancel();
                view.task = None;
                view.busy = false;
                view.table.update(cx, |table, cx| {
                    table.delegate_mut().preview = Some(preview);
                    table.delegate_mut().loading = false;
                    table.refresh(cx);
                });
                view
            })
        })
    })
    .map_err(Into::into)
}

/// Arrow keys select cells; Enter opens the selected value; Escape closes one layer at a time.
#[gpui_kit::test]
fn grid_navigation_and_inspector(cx: &mut TestAppContext) {
    let outcome = (|| -> Result<(), Box<dyn std::error::Error>> {
        let (handle, view) = fixture(cx)?;
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.press("down", cx);
            assert_eq!(view.read(cx).table.read(cx).selected_cell(), Some((0, 0)));
            window.press("right", cx);
            assert_eq!(view.read(cx).table.read(cx).selected_cell(), Some((0, 1)));
            window.press("enter", cx);
            assert!(view.read(cx).selected.is_some());
            window.press("escape", cx);
            assert!(view.read(cx).selected.is_none());
            assert_eq!(view.read(cx).table.read(cx).selected_cell(), Some((0, 1)));
            window.press("escape", cx);
            assert_eq!(view.read(cx).table.read(cx).selected_cell(), None);
            window.press("down", cx);
            window.press("space", cx);
            assert!(view.read(cx).selected.is_some());
        })?;
        Ok(())
    })();
    assert!(outcome.is_ok(), "{outcome:?}");
}

/// Find focuses the Browse input without executing drafts; closing all tabs restores Open files focus.
#[gpui_kit::test]
fn find_and_close_tabs(cx: &mut TestAppContext) {
    let outcome = (|| -> Result<(), Box<dyn std::error::Error>> {
        let (handle, view) = fixture(cx)?;
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let generation = view.read(cx).generation;
            window.press("secondary-f", cx);
            assert!(view.read(cx).search.focus_handle(cx).is_focused(window));
            assert_eq!(view.read(cx).generation, generation);
            window.press("secondary-enter", cx);
            assert_eq!(view.read(cx).generation, generation);
            assert_eq!(view.read(cx).active_tab().mode, TabMode::Browse);
            for remaining in (0..3).rev() {
                window.press("secondary-w", cx);
                assert_eq!(view.read(cx).tabs.len(), remaining);
            }
            assert!(view.read(cx).open_focus.is_focused(window));
        })?;
        Ok(())
    })();
    assert!(outcome.is_ok(), "{outcome:?}");
}

/// Clipboard shortcuts serialize the retained cell, while focused inputs retain native text copying.
#[gpui_kit::test]
async fn copy_shortcuts_respect_focus(cx: &mut TestAppContext) {
    use gpui_kit::test::TestAppContextExt;
    let outcome = async {
        let (handle, view) = fixture(cx)?;
        for (key, expected) in [
            ("secondary-c", "Olivia Chen"),
            ("secondary-shift-c", "\"Olivia Chen\""),
        ] {
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                view.update(cx, |view, cx| {
                    view.table
                        .update(cx, |table, cx| table.set_selected_cell(0, 1, cx));
                    view.focus_content(window, cx);
                });
                window.press(key, cx);
            })?;
            cx.wait_for(handle, std::time::Duration::from_secs(1), |_, cx| {
                cx.read_from_clipboard()
                    .and_then(|item| item.text())
                    .as_deref()
                    == Some(expected)
            })
            .await;
        }
        cx.update_window(handle, |_, window, cx| {
            window.press("secondary-f", cx);
            view.update(cx, |view, cx| {
                view.search
                    .update(cx, |input, cx| input.set_value("input text", window, cx))
            });
            window.press("secondary-a", cx);
            window.press("secondary-c", cx);
            assert_eq!(
                cx.read_from_clipboard()
                    .and_then(|item| item.text())
                    .as_deref(),
                Some("input text")
            );
            assert!(view.read(cx).search.focus_handle(cx).is_focused(window));
        })?;
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    assert!(outcome.is_ok(), "{outcome:?}");
}

/// SQL execution is mode-scoped and Find does not leave SQL or execute its draft.
#[gpui_kit::test]
fn sql_shortcuts_respect_mode(cx: &mut TestAppContext) {
    let outcome = (|| -> Result<(), Box<dyn std::error::Error>> {
        let (handle, view) = fixture(cx)?;
        cx.update_window(handle, |_, window, cx| {
            view.update(cx, |view, cx| {
                view.switch_mode(window, cx);
                view.sql.focus_handle(cx).focus(window, cx);
            });
            window.render_frame(cx);
            view.read(cx).sql.focus_handle(cx).focus(window, cx);
            let generation = view.read(cx).generation;
            window.press("secondary-f", cx);
            assert_eq!(view.read(cx).active_tab().mode, TabMode::Sql);
            assert!(!view.read(cx).search.focus_handle(cx).is_focused(window));
            assert_eq!(view.read(cx).generation, generation);
            window.press("secondary-enter", cx);
            assert!(view.read(cx).generation > generation);
        })?;
        Ok(())
    })();
    assert!(outcome.is_ok(), "{outcome:?}");
}
