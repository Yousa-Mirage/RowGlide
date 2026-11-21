//! Headless regression tests dispatch production key bindings through a real window.

use super::{Workspace, state::TabMode};
use gpui_kit::test::TestWindowExt;
use gpui_kit::{AppContext, Entity, Focusable, TestAppContext, WindowOptions};
use rowglide_core::{Backend, CacheLimits, Cancellation, MockBackend, Preview, Query, ResultStore};
use std::sync::Arc;

/// Open a production workspace with deterministic retained mock rows and no pending query.
pub(super) fn fixture(
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
            assert_eq!(window.find("filters-toggle").focused(), Some(true));
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

/// Invalid filter input never starts work; changing conditions resets deep offsets on success.
#[gpui_kit::test]
async fn filter_apply_preserves_old_page_until_success(cx: &mut TestAppContext) {
    use gpui_kit::test::TestAppContextExt;
    let outcome = async {
        let (window, view) = fixture(cx)?;
        cx.update_window(window, |_, window, cx| {
            view.update(cx, |view, cx| {
                view.active_tab_mut()
                    .drafts
                    .filters
                    .push(super::state::FilterDraft {
                        id: 1,
                        column: 3,
                        data_type: arrow_schema::DataType::Float64,
                        op: Some(rowglide_core::CompareOp::GreaterThan),
                        negated: false,
                        value: "invalid".into(),
                    });
                let rows = view
                    .table
                    .read(cx)
                    .delegate()
                    .preview
                    .as_ref()
                    .map(|p| p.rows);
                view.apply_filter(window, cx);
                assert!(!view.busy);
                assert!(view.error);
                assert_eq!(
                    view.table
                        .read(cx)
                        .delegate()
                        .preview
                        .as_ref()
                        .map(|p| p.rows),
                    rows
                );
                assert!(view.active_tab().browse.filters.is_empty());
                view.active_tab_mut().drafts.filters[0].value = "100".into();
                view.offset
                    .update(cx, |state, cx| state.set_value("5000", window, cx));
                view.apply_filter(window, cx);
                assert!(view.busy);
                assert_eq!(view.query.offset, 0);
                assert_eq!(
                    view.table
                        .read(cx)
                        .delegate()
                        .preview
                        .as_ref()
                        .map(|p| p.rows),
                    rows
                );
                assert!(view.active_tab().browse.filters.is_empty());
            });
        })?;
        cx.wait_for(window, std::time::Duration::from_secs(5), |_, cx| {
            !view.read(cx).busy
        })
        .await;
        cx.update(|cx| {
            let view = view.read(cx);
            assert!(!view.error, "{}", view.status);
            assert_eq!(view.active_tab().browse.filters.len(), 1);
            assert_eq!(view.active_tab().browse.offset, 0);
            assert_eq!(view.offset.read(cx).value().as_ref(), "0");
        });
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    assert!(outcome.is_ok(), "{outcome:?}");
}

/// Cancellation and backend errors retain the last page and its applied predicates.
#[gpui_kit::test]
async fn browse_failure_and_cancel_retain_results(cx: &mut TestAppContext) {
    use gpui_kit::test::TestAppContextExt;
    let outcome = async {
        let (handle, view) = fixture(cx)?;
        let rows = cx.update_window(handle, |_, window, cx| {
            view.update(cx, |view, cx| {
                let rows = view
                    .table
                    .read(cx)
                    .delegate()
                    .preview
                    .as_ref()
                    .map(|p| p.rows);
                view.search
                    .update(cx, |input, cx| input.set_value("London", window, cx));
                view.apply_filter(window, cx);
                view.cancel(cx);
                assert!(view.active_tab().browse.search.is_empty());
                assert_eq!(
                    view.table
                        .read(cx)
                        .delegate()
                        .preview
                        .as_ref()
                        .map(|p| p.rows),
                    rows
                );
                view.query.source = usize::MAX;
                view.run(window, cx);
                rows
            })
        })?;
        cx.wait_for(handle, std::time::Duration::from_secs(5), |_, cx| {
            !view.read(cx).busy
        })
        .await;
        cx.update_window(handle, |_, _, cx| {
            let view = view.read(cx);
            assert!(view.error);
            assert!(view.active_tab().browse.search.is_empty());
            assert_eq!(
                view.table
                    .read(cx)
                    .delegate()
                    .preview
                    .as_ref()
                    .map(|p| p.rows),
                rows
            );
        })?;
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    assert!(outcome.is_ok(), "{outcome:?}");
}

/// Completion and sorting preserve edits made after submission; only applied predicates execute.
#[gpui_kit::test]
async fn browse_completion_preserves_new_drafts(cx: &mut TestAppContext) {
    use gpui_kit::test::TestAppContextExt;
    let outcome = async {
        let (handle, view) = fixture(cx)?;
        cx.update_window(handle, |_, window, cx| {
            view.update(cx, |view, cx| {
                view.search
                    .update(cx, |input, cx| input.set_value(" London ", window, cx));
                view.apply_filter(window, cx);
                view.search
                    .update(cx, |input, cx| input.set_value("Tokyo", window, cx));
                view.offset
                    .update(cx, |input, cx| input.set_value("5000", window, cx));
            });
        })?;
        cx.wait_for(handle, std::time::Duration::from_secs(5), |_, cx| {
            !view.read(cx).busy
        })
        .await;
        cx.update_window(handle, |_, window, cx| {
            view.update(cx, |view, cx| {
                assert_eq!(view.active_tab().browse.search, "London");
                assert_eq!(view.browse_draft(cx).search, "Tokyo");
                assert!(!view.can_turn_page(true, cx));
                view.query = view.active_tab().browse.clone();
                view.query.sort = Some(rowglide_core::Sort {
                    column: 0,
                    descending: true,
                });
                view.query.offset = 0;
                view.run(window, cx);
            });
        })?;
        cx.wait_for(handle, std::time::Duration::from_secs(5), |_, cx| {
            !view.read(cx).busy
        })
        .await;
        cx.update_window(handle, |_, window, cx| {
            view.update(cx, |view, cx| {
                assert_eq!(view.browse_draft(cx).search, "Tokyo");
                assert_eq!(view.browse_draft(cx).offset, "5000");
                assert_eq!(view.active_tab().browse.search, "London");
                view.discard_filter_drafts(window, cx);
                view.offset
                    .update(cx, |input, cx| input.set_value("12", window, cx));
                view.apply_filter(window, cx);
                assert_eq!(view.query.offset, 12);
                view.cancel(cx);
            });
        })?;
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    assert!(outcome.is_ok(), "{outcome:?}");
}

/// Hiding every field keeps restoration available; inspecting a restored field uses its Arrow index.
#[gpui_kit::test]
fn restore_hidden_columns_in_schema_order(cx: &mut TestAppContext) {
    use crate::table::ColumnVisibility;
    let outcome = (|| -> Result<(), Box<dyn std::error::Error>> {
        let (handle, view) = fixture(cx)?;
        cx.update_window(handle, |_, window, cx| {
            let grid = view.read(cx).table.read(cx).delegate();
            let schema = grid.schema().ok_or("missing preview schema")?;
            let view_id = grid.view_id;
            view.update(cx, |view, cx| {
                for column in 0..schema.fields().len() {
                    view.change_column_visibility(
                        &ColumnVisibility {
                            view_id,
                            columns: [column].into(),
                            show: false,
                        },
                        cx,
                    );
                }
            });
            window.render_frame(cx);
            assert!(window.find("hidden-columns").visible());
            assert!(
                view.read(cx)
                    .table
                    .read(cx)
                    .delegate()
                    .copy_target(0, 0)
                    .is_none()
            );
            view.update(cx, |view, cx| {
                view.change_column_visibility(
                    &ColumnVisibility {
                        view_id,
                        columns: [1].into(),
                        show: true,
                    },
                    cx,
                );
                view.inspect_cell(0, 0, cx);
                assert_eq!(
                    view.selected.as_ref().map(|value| &value.0),
                    Some(schema.field(1).name())
                );
                view.change_column_visibility(
                    &ColumnVisibility {
                        view_id,
                        columns: [0].into(),
                        show: true,
                    },
                    cx,
                );
                let grid = view.table.read(cx).delegate();
                assert_eq!(grid.visible.as_deref(), Some([0, 1].as_slice()));
            });
            Ok::<_, Box<dyn std::error::Error>>(())
        })??;
        Ok(())
    })();
    assert!(outcome.is_ok(), "{outcome:?}");
}

/// A right click on a real table header exposes both directions and the field actions.
#[gpui_kit::test]
async fn right_click_header_opens_field_menu(cx: &mut TestAppContext) {
    use gpui_kit::test::{TestAppContextExt, TestQueryExt};
    let outcome = async {
        let (handle, _) = fixture(cx)?;
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.right_click(("col-header", 0usize), cx);
        })?;
        cx.wait_for(handle, std::time::Duration::from_secs(1), |window, _| {
            window.try_find("popup-menu").is_some()
        })
        .await;
        cx.update_window(handle, |_, window, _| {
            for key in [
                "copy.table",
                "copy.json",
                "table.ascending",
                "table.descending",
                "table.copy_name",
                "table.hide_column",
            ] {
                assert!(window.find_by_label(&crate::i18n::tr!(key)).visible());
            }
        })?;
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    assert!(outcome.is_ok(), "{outcome:?}");
}

/// Select backwards, toggle out the middle field, then use both batch menu actions.
#[gpui_kit::test]
async fn multiselect_header_actions(cx: &mut TestAppContext) {
    use crate::i18n::tr;
    use gpui_kit::{
        Modifiers, MouseButton,
        test::{TestAppContextExt, TestEventExt, TestQueryExt},
    };
    let outcome = async {
        let (handle, view) = fixture(cx)?;
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.click(("col-header", 2usize), cx);
            window.click_with_modifiers(
                ("col-header", 0usize),
                Modifiers {
                    shift: true,
                    ..Default::default()
                },
                cx,
            );
            let grid = view.read(cx).table.read(cx).delegate();
            assert_eq!(grid.header_columns, [0, 1, 2].into());
            let mac = cx.platform_info().is_mac();
            window.click_with_modifiers(
                ("col-header", 1usize),
                Modifiers {
                    platform: mac,
                    control: !mac,
                    ..Default::default()
                },
                cx,
            );
            assert_eq!(
                view.read(cx).table.read(cx).delegate().header_columns,
                [0, 2].into()
            );
        })?;
        // Right-clicking a data cell in a selected column keeps both nonadjacent columns.
        cx.update_window(handle, |_, window, cx| {
            window.right_click("table-cell-0:2", cx)
        })?;
        cx.wait_for(handle, std::time::Duration::from_secs(1), |window, _| {
            window.try_find_by_label(&tr!("copy.json")).is_some()
        })
        .await;
        let rows = cx.update_window(handle, |_, window, cx| {
            let point = window.find_by_label(&tr!("copy.json")).bounds().center();
            window.pointer_move(point, None, cx);
            window.pointer_down(point, MouseButton::Left, cx);
            window.pointer_up(point, MouseButton::Left, cx);
            view.read(cx)
                .table
                .read(cx)
                .delegate()
                .preview
                .as_ref()
                .map_or(0, |preview| preview.rows)
        })?;
        cx.wait_for(handle, std::time::Duration::from_secs(3), |_, cx| {
            cx.read_from_clipboard()
                .and_then(|item| item.text())
                .is_some_and(|text| text.starts_with("{\"id\":"))
        })
        .await;
        cx.update_window(
            handle,
            |_, _, cx| -> Result<(), Box<dyn std::error::Error>> {
                let text = cx
                    .read_from_clipboard()
                    .and_then(|item| item.text())
                    .ok_or("missing selection")?;
                let data: serde_json::Value = serde_json::from_str(&text)?;
                assert_eq!(data.as_object().map(|map| map.len()), Some(2));
                assert_eq!(data["id"].as_array().map(Vec::len), Some(rows));
                assert_eq!(data["city"].as_array().map(Vec::len), Some(rows));
                assert_eq!(data["id"][0], 10001);
                assert_eq!(data["city"][0], "Shanghai");
                Ok(())
            },
        )??;
        for action in [tr!("table.copy_names"), tr!("table.hide_columns")] {
            cx.update_window(handle, |_, window, cx| {
                window.right_click(("col-header", 0usize), cx)
            })?;
            cx.wait_for(handle, std::time::Duration::from_secs(1), |window, _| {
                window.try_find("popup-menu").is_some()
            })
            .await;
            cx.update_window(handle, |_, window, cx| {
                assert!(window.try_find_by_label(&tr!("table.ascending")).is_none());
                assert!(window.try_find_by_label(&tr!("table.descending")).is_none());
                let point = window.find_by_label(&action).bounds().center();
                window.pointer_move(point, None, cx);
                window.pointer_down(point, MouseButton::Left, cx);
                window.pointer_up(point, MouseButton::Left, cx);
            })?;
            cx.run_until_parked();
        }
        cx.update_window(handle, |_, _, cx| {
            assert_eq!(
                cx.read_from_clipboard()
                    .and_then(|item| item.text())
                    .as_deref(),
                Some("id\ncity")
            );
            let grid = view.read(cx).table.read(cx).delegate();
            assert_eq!(grid.schema_index(0), Some(1));
            assert_eq!(grid.schema_index(1), Some(3));
        })?;
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    assert!(outcome.is_ok(), "{outcome:?}");
}

/// Drag and Shift-click both select typed rectangles and publish the appropriate clipboard format.
#[gpui_kit::test]
async fn rectangular_cell_copy(cx: &mut TestAppContext) {
    use crate::i18n::tr;
    use gpui_kit::{
        Modifiers, MouseButton,
        test::{TestAppContextExt, TestEventExt, TestQueryExt},
    };
    let outcome = async {
        let (handle, view) = fixture(cx)?;
        for (drag, label, expected) in [
            (
                true,
                tr!("copy.table"),
                "id\tname\n10001\tOlivia Chen\n10002\tNoah Williams",
            ),
            (
                false,
                tr!("copy.json"),
                r#"{"id":[10001,10002],"name":["Olivia Chen","Noah Williams"]}"#,
            ),
        ] {
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                if drag {
                    window.drag_to("table-cell-1:1", "table-cell-0:0", cx);
                } else {
                    window.click("table-cell-0:0", cx);
                    window.click_with_modifiers(
                        "table-cell-1:1",
                        Modifiers {
                            shift: true,
                            ..Default::default()
                        },
                        cx,
                    );
                }
                let range = view.read(cx).table.read(cx).delegate().cell_range;
                assert!(
                    range.is_some_and(|r| r.includes(0, 1) && r.includes(1, 0)),
                    "drag={drag}"
                );
                window.right_click("table-cell-0:1", cx);
                assert!(
                    view.read(cx)
                        .table
                        .read(cx)
                        .delegate()
                        .cell_range
                        .is_some_and(|r| r.includes(0, 1)),
                    "after right click drag={drag}"
                );
            })?;
            cx.wait_for(handle, std::time::Duration::from_secs(2), |w, _| {
                w.try_find_by_label(&label).is_some()
            })
            .await;
            cx.update_window(handle, |_, window, cx| {
                assert!(
                    window.try_find_by_label(&tr!("copy.value")).is_none(),
                    "menu drag={drag}"
                );
                let point = window.find_by_label(&label).bounds().center();
                window.pointer_move(point, None, cx);
                window.pointer_down(point, MouseButton::Left, cx);
                window.pointer_up(point, MouseButton::Left, cx);
            })?;
            cx.wait_for(handle, std::time::Duration::from_secs(2), |_, cx| {
                cx.read_from_clipboard()
                    .and_then(|item| item.text())
                    .as_deref()
                    == Some(expected)
            })
            .await;
        }
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    assert!(outcome.is_ok(), "{outcome:?}");
}
