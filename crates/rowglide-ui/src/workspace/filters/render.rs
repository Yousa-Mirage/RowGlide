//! Compact filter panel layout using the existing GPUI Kit controls.

use super::{Edit, operator_label};
use crate::{
    i18n::{tr, tr_string},
    workspace::{Workspace, state::FilterDraft},
};
use gpui_kit::component::{
    ActiveTheme, Disableable, IconName, Sizable,
    button::{Button, ButtonVariants},
    input::Input,
    menu::{DropdownMenu, PopupMenuItem},
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use rowglide_core::{FilterKind, filter_kind, filter_operators};

impl Workspace {
    /// Build a height-bounded, scrollable editor; every control modifies drafts only.
    pub(in crate::workspace) fn render_filters(&self, cx: &mut Context<Self>) -> AnyElement {
        let tab = self.active_tab();
        let blocked = tab.changed || self.reloading == Some(tab.source.id);
        let dirty = !self.browse_draft(cx).matches_query(&tab.browse);
        let can_add = !blocked
            && tab
                .source
                .schema
                .fields()
                .iter()
                .any(|field| filter_kind(field.data_type()).is_some());
        div()
            .key_context("RowglideBrowse")
            .px_5()
            .py_2()
            .flex()
            .flex_col()
            .gap_2()
            .flex_shrink_0()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().text_xs().child(tr!("filters.search")))
                    .child(
                        div().flex_1().max_w(px(400.)).child(
                            Input::new(&self.search)
                                .id("browse-filter")
                                .aria_label(tr!("a11y.filter"))
                                .small()
                                .disabled(blocked || !self.has_search_columns()),
                        ),
                    )
                    .when(!self.has_search_columns(), |s| {
                        s.child(div().text_xs().child(tr!("filters.no_text")))
                    })
                    .when(dirty, |s| {
                        s.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(tr!("filters.unapplied")),
                        )
                    }),
            )
            .when(!tab.drafts.filters.is_empty(), |panel| {
                panel.child(
                    div()
                        .flex()
                        .gap_2()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(div().w(px(220.)).child(tr!("filters.column")))
                        .child(div().w(px(144.)).child(tr!("filters.operator")))
                        .child(div().flex_1().child(tr!("filters.value"))),
                )
            })
            .child(
                div()
                    .id("filter-rows")
                    .test_support()
                    .max_h(px(224.))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .children(
                        tab.drafts
                            .filters
                            .iter()
                            .map(|row| self.render_filter_row(row, blocked, cx)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .flex_wrap()
                    .child(
                        Button::new("add-filter")
                            .small()
                            .label(tr!("filters.add"))
                            .disabled(!can_add)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.add_filter(window, cx)),
                            ),
                    )
                    .child(
                        Button::new("clear-filters")
                            .small()
                            .ghost()
                            .label(tr!("filters.clear"))
                            .disabled(blocked || tab.drafts.filters.is_empty())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.active_tab_mut().drafts.filters.clear();
                                this.filter_editors.clear();
                                cx.notify();
                            })),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("discard-filters")
                            .small()
                            .label(tr!("common.cancel"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.discard_filter_drafts(window, cx)
                            })),
                    )
                    .child(
                        Button::new("apply-filters")
                            .small()
                            .primary()
                            .label(tr!("common.apply"))
                            .disabled(blocked)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.apply_filter(window, cx)),
                            ),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr!("filters.help")),
            )
            .into_any_element()
    }

    /// Render one stable row with type-dependent operators and a raw or boolean value editor.
    fn render_filter_row(
        &self,
        row: &FilterDraft,
        blocked: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let source = self.active_tab().source.id;
        let id = row.id;
        let schema = self.active_tab().source.schema.clone();
        let column = row.column;
        let label = schema
            .fields()
            .get(column)
            .map(|field| format!("{} · {}", field.name(), field.data_type()))
            .unwrap_or_else(|| tr_string!("filters.column"));
        let weak = cx.entity().downgrade();
        let columns = Button::new(("filter-column", id))
            .small()
            .label(label)
            .accessibility_label(tr!("filters.column"))
            .disabled(blocked)
            .dropdown_menu(move |mut menu, _, _| {
                menu = menu.scrollable(true);
                for (index, field) in schema.fields().iter().enumerate() {
                    if filter_kind(field.data_type()).is_none() {
                        continue;
                    }
                    let weak = weak.clone();
                    menu = menu.item(
                        PopupMenuItem::new(format!("{} · {}", field.name(), field.data_type()))
                            .checked(index == column)
                            .on_click(move |_, window, cx| {
                                let _ = weak.update(cx, |this, cx| {
                                    this.edit_filter(source, id, Edit::Column(index), window, cx)
                                });
                            }),
                    );
                }
                menu
            });
        let choices = filter_operators(&row.data_type)
            .iter()
            .map(|op| (Some(*op), false))
            .chain([(None, false), (None, true)])
            .collect::<Vec<_>>();
        let current = (row.op, row.negated);
        let weak = cx.entity().downgrade();
        let operators = Button::new(("filter-operator", id))
            .small()
            .label(operator_label(row.op, row.negated))
            .accessibility_label(tr!("filters.operator"))
            .disabled(blocked)
            .dropdown_menu(move |mut menu, _, _| {
                for &(op, negated) in &choices {
                    let weak = weak.clone();
                    menu = menu.item(
                        PopupMenuItem::new(operator_label(op, negated))
                            .checked((op, negated) == current)
                            .on_click(move |_, window, cx| {
                                let _ = weak.update(cx, |this, cx| {
                                    this.edit_filter(
                                        source,
                                        id,
                                        Edit::Operator(op, negated),
                                        window,
                                        cx,
                                    )
                                });
                            }),
                    );
                }
                menu
            });
        let mut value = div().flex_1().min_w(px(140.));
        if row.op.is_some() {
            if filter_kind(&row.data_type) == Some(FilterKind::Boolean) {
                let weak = cx.entity().downgrade();
                let selected = row.value == "true";
                value = value.child(
                    Button::new(("filter-boolean", id))
                        .small()
                        .label(if selected {
                            tr!("filters.true")
                        } else {
                            tr!("filters.false")
                        })
                        .accessibility_label(tr!("filters.value"))
                        .disabled(blocked)
                        .dropdown_menu(move |mut menu, _, _| {
                            for boolean in [true, false] {
                                let weak = weak.clone();
                                menu = menu.item(
                                    PopupMenuItem::new(if boolean {
                                        tr!("filters.true")
                                    } else {
                                        tr!("filters.false")
                                    })
                                    .checked(selected == boolean)
                                    .on_click(
                                        move |_, window, cx| {
                                            let _ = weak.update(cx, |this, cx| {
                                                this.edit_filter(
                                                    source,
                                                    id,
                                                    Edit::Boolean(boolean),
                                                    window,
                                                    cx,
                                                )
                                            });
                                        },
                                    ),
                                );
                            }
                            menu
                        }),
                );
            } else if let Some(editor) = self.filter_editors.iter().find(|editor| editor.id == id) {
                value = value.child(
                    Input::new(&editor.input)
                        .small()
                        .aria_label(tr!("filters.value"))
                        .disabled(blocked),
                );
            }
        }
        div()
            .id(("filter-row", id))
            .test_support()
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap_2()
            .flex_wrap()
            .child(div().w(px(220.)).overflow_hidden().child(columns))
            .child(div().w(px(144.)).child(operators))
            .child(value)
            .child(
                Button::new(("remove-filter", id))
                    .small()
                    .ghost()
                    .icon(IconName::Close)
                    .accessibility_label(tr!("filters.remove"))
                    .tooltip(tr!("filters.remove"))
                    .disabled(blocked)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.active_tab_mut()
                            .drafts
                            .filters
                            .retain(|row| row.id != id);
                        this.filter_editors.retain(|editor| editor.id != id);
                        cx.notify();
                    })),
            )
            .into_any_element()
    }
}
