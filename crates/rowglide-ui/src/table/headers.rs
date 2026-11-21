//! Actions on the original field represented by a visible column header.

use super::{ColumnVisibility, Grid, SortRequested, sizing};
use crate::i18n::tr;
use gpui_kit::component::{
    menu::{ContextMenuExt, PopupMenuItem},
    table::TableState,
};
use gpui_kit::*;
use rowglide_core::Sort;

impl Grid {
    /// Build the menu from the applied sort and capture the unformatted Arrow field name.
    pub(super) fn header_menu(&self, display: usize, cx: &Context<TableState<Self>>) -> AnyElement {
        let Some((column, field)) = self.field(display) else {
            return div().into_any_element();
        };
        let name = field.name().clone();
        let sortable = self.sortable && sizing::sortable(&field);
        let applied = self.sort;
        let events = self.events.clone();
        let table = cx.weak_entity();
        let view_id = self.view_id;
        div()
            .id(("column-header", column))
            .w_full()
            .child(sizing::header(&field))
            .context_menu(move |mut menu, _, _| {
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
                                    events.update(cx, |_, cx| cx.emit(SortRequested(Some(sort))));
                                }
                            }),
                    );
                }
                let name = name.clone();
                let events = events.clone();
                menu.separator()
                    .item(
                        PopupMenuItem::new(tr!("table.copy_name")).on_click(move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(name.clone()));
                        }),
                    )
                    .item(
                        PopupMenuItem::new(tr!("table.hide_column")).on_click(move |_, _, cx| {
                            events.update(cx, |_, cx| {
                                cx.emit(ColumnVisibility {
                                    view_id,
                                    column,
                                    show: false,
                                })
                            });
                        }),
                    )
            })
            .into_any_element()
    }
}
