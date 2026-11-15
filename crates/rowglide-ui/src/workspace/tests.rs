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
                let mut view =
                    Workspace::new(Arc::new(MockBackend), store, Vec::new(), None, window, cx);
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
        let text = "Full value: 汉字 \"quoted\" ".repeat(200);
        let batch = arrow_array::RecordBatch::try_from_iter([(
            "value",
            Arc::new(arrow_array::StringArray::from(vec![text.as_str()])) as arrow_array::ArrayRef,
        )])?;
        cx.update_window(handle, |_, _, cx| {
            view.update(cx, |view, cx| {
                view.table.update(cx, |table, cx| {
                    table.delegate_mut().preview = Some(Preview {
                        schema: batch.schema(),
                        batches: vec![batch],
                        rows: 1,
                        total_rows: Some(1),
                        truncated: false,
                    });
                    table.refresh(cx);
                })
            });
        })?;
        let json = serde_json::to_string(&text)?;
        for (key, expected) in [
            ("secondary-c", text.as_str()),
            ("secondary-shift-c", json.as_str()),
        ] {
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                view.update(cx, |view, cx| {
                    view.table
                        .update(cx, |table, cx| table.set_selected_cell(0, 0, cx));
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

/// The tab group is one stop; arrows switch files and toolbar traversal exposes localized names.
#[gpui_kit::test]
fn tab_group_and_toolbar_focus(cx: &mut TestAppContext) {
    use gpui_kit::test::TestQueryExt;
    let outcome = (|| -> Result<(), Box<dyn std::error::Error>> {
        let (handle, view) = fixture(cx)?;
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.within("open-file-tabs").click(0usize, cx);
            window.press("right", cx);
            assert_eq!(view.read(cx).active_tab, 1);
            assert!(view.read(cx).tabs_focus.is_focused(window));
            window.press("home", cx);
            assert_eq!(view.read(cx).active_tab, 0);
            window.press("tab", cx);
            assert!(view.read(cx).open_focus.is_focused(window));
            window.press("tab", cx);
            assert!(view.read(cx).search.focus_handle(cx).is_focused(window));
            window.press("tab", cx);
            assert!(view.read(cx).limit.focus_handle(cx).is_focused(window));
            window.press("tab", cx);
            assert!(view.read(cx).offset.focus_handle(cx).is_focused(window));
            window.press("shift-tab", cx);
            assert!(view.read(cx).limit.focus_handle(cx).is_focused(window));
            window.press("secondary-f", cx);
            assert!(view.read(cx).search.focus_handle(cx).is_focused(window));
            assert!(
                window
                    .find_by_label(&crate::i18n::tr!("a11y.filter"))
                    .visible()
            );
            assert!(
                window
                    .find_by_label(&crate::i18n::tr!("a11y.data_table"))
                    .visible()
            );
            assert!(
                window
                    .find_by_label(&crate::i18n::tr!("files.close", name = "customers.parquet"))
                    .visible()
            );
        })?;
        Ok(())
    })();
    assert!(outcome.is_ok(), "{outcome:?}");
}

/// Inspector disclosures activate with Enter/Space and closing from a control restores grid focus.
#[gpui_kit::test]
fn inspector_disclosure_and_focus_restore(cx: &mut TestAppContext) {
    let outcome = (|| -> Result<(), Box<dyn std::error::Error>> {
        let (handle, view) = fixture(cx)?;
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            view.update(cx, |view, cx| {
                view.table
                    .update(cx, |table, cx| table.set_selected_cell(0, 6, cx))
            });
            window.press("enter", cx);
            window.press("tab", cx);
            assert_eq!(window.find("close-inspector").focused(), Some(true));
            window.press("tab", cx);
            assert_eq!(window.find("expand-value").focused(), Some(true));
            window.press("space", cx);
            assert!(!view.read(cx).expanded.contains("value"));
            window.press("enter", cx);
            assert!(view.read(cx).expanded.contains("value"));
            window.press("escape", cx);
            assert!(view.read(cx).selected.is_none());
            assert!(view.read(cx).table.focus_handle(cx).is_focused(window));
            window.press("enter", cx);
            window.click("close-inspector", cx);
            assert!(view.read(cx).table.focus_handle(cx).is_focused(window));
        })?;
        Ok(())
    })();
    assert!(outcome.is_ok(), "{outcome:?}");
}

/// SQL Tab/Shift-Tab leave the editor and Escape from a text input dismisses the inspector.
#[gpui_kit::test]
fn editor_traversal_and_input_escape(cx: &mut TestAppContext) {
    let outcome = (|| -> Result<(), Box<dyn std::error::Error>> {
        let (handle, view) = fixture(cx)?;
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.press("down", cx);
            window.press("enter", cx);
            window.press("secondary-f", cx);
            window.press("escape", cx);
            assert!(view.read(cx).selected.is_none());
            assert!(view.read(cx).table.focus_handle(cx).is_focused(window));
            view.update(cx, |view, cx| view.switch_mode(window, cx));
            window.render_frame(cx);
            view.read(cx).sql.focus_handle(cx).focus(window, cx);
            window.press("tab", cx);
            assert!(view.read(cx).table.focus_handle(cx).is_focused(window));
            window.press("shift-tab", cx);
            assert!(view.read(cx).sql.focus_handle(cx).is_focused(window));
            window.press("shift-tab", cx);
            assert_eq!(window.find("run").focused(), Some(true));
        })?;
        Ok(())
    })();
    assert!(outcome.is_ok(), "{outcome:?}");
}

/// A context menu consumes Escape before the workspace closes the visible inspector.
#[gpui_kit::test]
async fn escape_dismisses_menu_before_inspector(cx: &mut TestAppContext) {
    use gpui_kit::test::{ClickOptions, TestAppContextExt, TestEventExt};
    let outcome = async {
        let (handle, view) = fixture(cx)?;
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.press("down", cx);
            window.press("enter", cx);
            window.click_with_options(
                "table-cell-0:0",
                ClickOptions::new().with_button(gpui_kit::MouseButton::Right),
                cx,
            );
        })?;
        cx.wait_for(handle, std::time::Duration::from_secs(1), |window, _| {
            window.try_find("popup-menu").is_some()
        })
        .await;
        cx.update_window(handle, |_, window, cx| {
            window.key_down("escape", false, cx);
            assert!(view.read(cx).selected.is_some());
        })?;
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| window.key_up("escape", cx))?;
        cx.wait_for(handle, std::time::Duration::from_secs(1), |window, _| {
            window.try_find("popup-menu").is_none()
        })
        .await;
        cx.update_window(handle, |_, window, cx| {
            window.press("escape", cx);
            assert!(view.read(cx).selected.is_none());
        })?;
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    assert!(outcome.is_ok(), "{outcome:?}");
}

/// File information has its own keyboard tab group; Escape restores the main table's focus.
#[gpui_kit::test]
fn file_info_keyboard_and_dismissal(cx: &mut TestAppContext) {
    let outcome = (|| -> Result<(), Box<dyn std::error::Error>> {
        let (handle, view) = fixture(cx)?;
        cx.update_window(handle, |_, window, cx| {
            view.update(cx, |view, cx| {
                view.active_tab_mut().source.parquet = Some(rowglide_core::ParquetSummary {
                    file_bytes: 1024,
                    columns: 7,
                    row_groups: 1,
                    created_by: None,
                    format_version: 2,
                });
                cx.notify();
            });
            window.render_frame(cx);
            window.click("file-info-toggle", cx);
            assert!(view.read(cx).active_tab().info.open);
            window.within("file-info-sections").click(0usize, cx);
            window.press("right", cx);
            assert!(view.read(cx).info_tabs_focus.is_focused(window));
            window.press("home", cx);
            window.press("escape", cx);
            assert!(!view.read(cx).active_tab().info.open);
            assert!(view.read(cx).table.focus_handle(cx).is_focused(window));
        })?;
        Ok(())
    })();
    assert!(outcome.is_ok(), "{outcome:?}");
}

/// Empty and replaced results do not allow stale keyboard selections to open or copy a cell.
#[gpui_kit::test]
fn empty_grid_keyboard_is_safe(cx: &mut TestAppContext) {
    let outcome = (|| -> Result<(), Box<dyn std::error::Error>> {
        let (handle, view) = fixture(cx)?;
        cx.update_window(handle, |_, window, cx| {
            view.update(cx, |view, cx| {
                view.table.update(cx, |table, cx| {
                    table.delegate_mut().preview = None;
                    table.clear_selection(cx);
                    table.refresh(cx);
                })
            });
            window.render_frame(cx);
            window.press("down", cx);
            window.press("right", cx);
            window.press("enter", cx);
            window.press("space", cx);
            window.press("secondary-c", cx);
            assert!(view.read(cx).selected.is_none());
            assert!(view.read(cx).clipboard.message.is_none());
            assert!(view.read(cx).table.read(cx).selected_cell().is_none());
        })?;
        Ok(())
    })();
    assert!(outcome.is_ok(), "{outcome:?}");
}

/// Keyboard selection reveals offscreen file tabs and returns to the beginning with Home.
#[gpui_kit::test]
fn tab_navigation_reveals_overflow(cx: &mut TestAppContext) {
    let outcome = (|| -> Result<(), Box<dyn std::error::Error>> {
        let (handle, view) = fixture(cx)?;
        cx.update_window(handle, |_, window, cx| {
            view.update(cx, |view, cx| {
                for tab in &mut view.tabs {
                    tab.source.name = tab.source.name.repeat(4);
                }
                cx.notify();
            });
            window.render_frame(cx);
            view.read(cx).tabs_focus.clone().focus(window, cx);
            window.press("end", cx);
            assert_eq!(view.read(cx).active_tab, 2);
            assert!(view.read(cx).tabs_scroll.offset().x < gpui_kit::px(0.));
            window.press("home", cx);
            assert_eq!(view.read(cx).active_tab, 0);
            let viewport = view.read(cx).tabs_scroll.bounds();
            let first = window.within("open-file-tabs").find(0usize).bounds();
            assert!(first.left() >= viewport.left());
            assert!(first.right() <= viewport.right());
        })?;
        Ok(())
    })();
    assert!(outcome.is_ok(), "{outcome:?}");
}
