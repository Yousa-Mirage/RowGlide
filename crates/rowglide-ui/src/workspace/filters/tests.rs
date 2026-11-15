//! Headless editor lifecycle and control-dispatch coverage.

use super::Edit;
use crate::workspace::tests::fixture;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{AppContext, Focusable, TestAppContext};
use rowglide_core::{CompareOp, FilterCondition, FilterKind, filter_kind};

/// Button dispatch adds/removes stable rows; column selection changes value type and null editing.
#[gpui_kit::test]
fn editor_controls_and_cancel(cx: &mut TestAppContext) {
    let outcome = (|| -> Result<(), Box<dyn std::error::Error>> {
        let (handle, view) = fixture(cx)?;
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.click("filters-toggle", cx);
            assert!(view.read(cx).active_tab().filters_open);
            window.click("add-filter", cx);
            let id = view.read(cx).active_tab().drafts.filters[0].id;
            view.update(cx, |this, cx| {
                this.edit_filter(0, id, Edit::Column(4), window, cx);
                let row = &this.active_tab().drafts.filters[0];
                assert_eq!(filter_kind(&row.data_type), Some(FilterKind::Boolean));
                assert_eq!(row.value, "true");
                this.edit_filter(0, id, Edit::Boolean(false), window, cx);
                assert_eq!(
                    this.active_tab().drafts.filters[0].condition(),
                    Ok(FilterCondition::Compare {
                        column: 4,
                        op: CompareOp::Equals,
                        value: rowglide_core::FilterValue::Boolean(false),
                    })
                );
                this.edit_filter(0, id, Edit::Operator(None, false), window, cx);
                assert_eq!(
                    this.active_tab().drafts.filters[0].condition(),
                    Ok(FilterCondition::IsNull {
                        column: 4,
                        negated: false
                    })
                );
            });
            window.click(("remove-filter", id), cx);
            assert!(view.read(cx).active_tab().drafts.filters.is_empty());
            window.click("add-filter", cx);
            assert_ne!(view.read(cx).active_tab().drafts.filters[0].id, id);
            window.click("discard-filters", cx);
            assert!(view.read(cx).active_tab().drafts.filters.is_empty());
            assert!(!view.read(cx).active_tab().filters_open);
            assert!(view.read(cx).table.focus_handle(cx).is_focused(window));
        })?;
        Ok(())
    })();
    assert!(outcome.is_ok(), "{outcome:?}");
}

/// Raw unfinished literals and panel preferences survive mode/tab switches without applying.
#[gpui_kit::test]
fn drafts_survive_sessions_and_are_bounded(cx: &mut TestAppContext) {
    let outcome = (|| -> Result<(), Box<dyn std::error::Error>> {
        let (handle, view) = fixture(cx)?;
        cx.update_window(handle, |_, window, cx| {
            view.update(cx, |this, cx| {
                this.active_tab_mut().filters_open = true;
                this.add_filter(window, cx);
                let input = this.filter_editors[0].input.clone();
                input.update(cx, |input, cx| input.set_value("-", window, cx));
                this.switch_mode(window, cx);
                this.switch_mode(window, cx);
                assert_eq!(this.filter_drafts(cx)[0].value, "-");
                this.switch_tab(1, window, cx);
                assert!(this.active_tab().drafts.filters.is_empty());
                this.switch_tab(0, window, cx);
                assert!(this.active_tab().filters_open);
                assert_eq!(this.filter_drafts(cx)[0].value, "-");
                assert!(this.active_tab().browse.filters.is_empty());
                for _ in 0..40 {
                    this.add_filter(window, cx);
                }
                assert_eq!(
                    this.active_tab().drafts.filters.len(),
                    rowglide_core::MAX_FILTERS
                );
                assert_eq!(this.filter_editors.len(), rowglide_core::MAX_FILTERS);
            });
            window.render_frame(cx);
            let first = view.read(cx).active_tab().drafts.filters[0].id;
            let last = view.read(cx).active_tab().drafts.filters[rowglide_core::MAX_FILTERS - 1].id;
            let viewport = window.find("filter-rows").bounds();
            assert!(viewport.size.height <= gpui_kit::px(224.));
            assert!(window.find(("filter-row", first)).bounds().size.height >= gpui_kit::px(24.));
            assert!(window.find(("filter-row", last)).bounds().bottom() > viewport.bottom());
        })?;
        Ok(())
    })();
    assert!(outcome.is_ok(), "{outcome:?}");
}

/// Find exposes the panel but never focuses a disabled search input for numeric-only sources.
#[gpui_kit::test]
fn no_text_search_is_not_focused(cx: &mut TestAppContext) {
    let outcome =
        (|| -> Result<(), Box<dyn std::error::Error>> {
            let (handle, view) = fixture(cx)?;
            cx.update_window(handle, |_, window, cx| {
                view.update(cx, |this, _| {
                    this.active_tab_mut().source.schema =
                        std::sync::Arc::new(arrow_schema::Schema::new(vec![
                            arrow_schema::Field::new("id", arrow_schema::DataType::Int64, false),
                        ]));
                });
                window.render_frame(cx);
                window.press("secondary-f", cx);
                assert!(view.read(cx).active_tab().filters_open);
                assert!(!view.read(cx).search.focus_handle(cx).is_focused(window));
                assert!(!view.read(cx).has_search_columns());
            })?;
            Ok(())
        })();
    assert!(outcome.is_ok(), "{outcome:?}");
}
