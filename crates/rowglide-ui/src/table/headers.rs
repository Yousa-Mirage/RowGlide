//! Actions on the original field represented by a visible column header.

use super::{ColumnVisibility, Grid, SortRequested, sizing};
use crate::{
    clipboard::{range::RangeCopy, range_menu},
    i18n::tr,
};
use gpui_kit::component::{
    ActiveTheme,
    menu::{ContextMenuExt, PopupMenuItem},
    table::TableState,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use rowglide_core::Sort;

impl Grid {
    /// Drop the header selection and range anchor when the view or column layout changes.
    pub(crate) fn clear_headers(&mut self) {
        self.header_columns.clear();
        self.header_anchor = None;
    }

    /// Apply a plain click, additive toggle, or anchored range in visible column order.
    fn select_header(&mut self, display: usize, range: bool, additive: bool) {
        let Some(column) = self.schema_index(display) else {
            return;
        };
        if !additive {
            self.header_columns.clear();
        }
        if range {
            let anchor = *self.header_anchor.get_or_insert(display);
            for position in anchor.min(display)..=anchor.max(display) {
                if let Some(index) = self.schema_index(position) {
                    self.header_columns.insert(index);
                }
            }
        } else {
            self.header_anchor = Some(display);
            if !additive || !self.header_columns.remove(&column) {
                self.header_columns.insert(column);
            }
        }
    }

    /// Build the menu from the applied sort and capture the unformatted Arrow field name.
    pub(super) fn header_menu(&self, display: usize, cx: &Context<TableState<Self>>) -> AnyElement {
        let Some((column, field)) = self.field(display) else {
            return div().into_any_element();
        };

        let sortable = self.sortable && sizing::sortable(&field);
        let applied = self.sort;
        let events = self.events.clone();
        let table = cx.weak_entity();
        let view_id = self.view_id;
        div()
            .id(("column-header", column))
            .size_full()
            .flex()
            .items_center()
            .when(self.header_columns.contains(&column), |header| {
                header.bg(cx.theme().table_active)
            })
            .on_click(cx.listener(move |table, click: &ClickEvent, window, cx| {
                if !click.standard_click() {
                    return;
                }
                let modifiers = click.modifiers();
                let additive = modifiers.secondary(cx);
                table.delegate_mut().clear_cell_range();
                table.clear_selection(cx);
                table
                    .delegate_mut()
                    .select_header(display, modifiers.shift, additive);
                table.focus_handle(cx).focus(window, cx);
                cx.stop_propagation();
                cx.notify();
            }))
            .child(sizing::header(&field))
            .context_menu(move |mut menu, _, cx| {
                let Some(entity) = table.upgrade() else {
                    return menu;
                };
                let Some((columns, schema, data)) = entity.update(cx, |state, cx| {
                    let grid = state.delegate_mut();
                    if grid.view_id != view_id || grid.schema_index(display) != Some(column) {
                        return None;
                    }
                    if !grid.header_columns.contains(&column) {
                        grid.select_header(display, false, false);
                        state.delegate_mut().clear_cell_range();
                        state.clear_selection(cx);
                    }
                    let grid = state.delegate();
                    Some((
                        grid.header_columns.clone(),
                        grid.schema()?,
                        RangeCopy::capture(grid, cx),
                    ))
                }) else {
                    return menu;
                };
                let multiple = columns.len() > 1;
                if !multiple {
                    for (descending, label) in [
                        (false, tr!("table.ascending")),
                        (true, tr!("table.descending")),
                    ] {
                        let sort = Sort { column, descending };
                        let table = table.clone();
                        let events = events.clone();
                        menu = menu.item(
                            PopupMenuItem::new(label)
                                .checked(applied == Some(sort))
                                .disabled(!sortable)
                                .on_click(move |_, _, cx| {
                                    if table
                                        .upgrade()
                                        .is_some_and(|t| t.read(cx).delegate().view_id == view_id)
                                    {
                                        events
                                            .update(cx, |_, cx| cx.emit(SortRequested(Some(sort))));
                                    }
                                }),
                        );
                    }
                    menu = menu.separator();
                }
                if let Some(data) = data {
                    menu = range_menu(menu, data, events.clone()).separator();
                }
                let name = columns
                    .iter()
                    .map(|&i| schema.field(i).name().as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                let events = events.clone();
                menu.item(
                    PopupMenuItem::new(if multiple {
                        tr!("table.copy_names")
                    } else {
                        tr!("table.copy_name")
                    })
                    .on_click(move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(name.clone()));
                    }),
                )
                .item(
                    PopupMenuItem::new(if multiple {
                        tr!("table.hide_columns")
                    } else {
                        tr!("table.hide_column")
                    })
                    .on_click(move |_, _, cx| {
                        events.update(cx, |_, cx| {
                            cx.emit(ColumnVisibility {
                                view_id,
                                columns: columns.clone(),
                                show: false,
                            })
                        });
                    }),
                )
            })
            .into_any_element()
    }
}
