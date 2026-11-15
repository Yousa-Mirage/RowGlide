//! File navigation, browse controls, SQL editor, and status bar.

use crate::i18n::{tr, tr_string};

use super::{TabMode, Workspace};
use gpui_kit::component::{
    ActiveTheme, Disableable, IconName, Sizable,
    button::{Button, ButtonVariants},
    input::{Editor, Input},
    tab::{Tab, TabBar},
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use rowglide_core::{BrowsePage, Query};

impl Workspace {
    /// Build Folder Tabs for registered sources and select the active file.
    ///
    /// Clicking another tab restores its session. Each close button uses the stable
    /// source ID and stops propagation so closing does not also select a shifted tab.
    pub(super) fn render_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        TabBar::new("open-file-tabs")
            .track_scroll(&self.tabs_scroll)
            .folder()
            .selected_index(self.active_tab)
            .children(self.tabs.iter().enumerate().map(|(index, tab)| {
                let source = tab.source.id;
                Tab::new()
                    .label(if tab.changed {
                        tr_string!("files.changed_tab", name = tab.source.name)
                    } else {
                        tab.source.name.clone()
                    })
                    .when(index == self.active_tab, |tab| {
                        tab.track_focus(&self.tabs_focus).tab_stop(true)
                    })
                    .focus_visible(|style| style.border_1().border_color(cx.theme().ring))
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                        if !this.tabs_focus.is_focused(window)
                            || event.keystroke.modifiers.modified()
                        {
                            return;
                        }
                        if let Some(index) = super::actions::tab_destination(
                            &event.keystroke.key,
                            this.active_tab,
                            this.tabs.len(),
                        ) {
                            this.switch_tab(index, window, cx);
                            this.tabs_scroll.scroll_to_item(index);
                            cx.notify();
                            this.tabs_focus.focus(window, cx);
                            cx.stop_propagation();
                        }
                    }))
                    .suffix(
                        Button::new(("close-file-tab", source))
                            .xsmall()
                            .ghost()
                            .icon(IconName::Close)
                            .tab_stop(false)
                            .accessibility_label(tr!("files.close", name = tab.source.name))
                            .tooltip(tr!("files.close", name = tab.source.name))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.close_tab(source, window, cx);
                                this.focus_content(window, cx);
                            })),
                    )
            }))
            .on_click(cx.listener(|this, index: &usize, window, cx| {
                this.switch_tab(*index, window, cx);
                this.tabs_focus.focus(window, cx);
            }))
    }

    /// Build the compact Browse filter, Limit/Offset, Apply/Reset and SQL toggle row.
    ///
    /// SQL mode disables Browse inputs and actions. Apply validates drafts before
    /// querying; Reset restores the default first page. Toggling modes restores the
    /// other mode's independent state without automatically executing SQL.
    pub(super) fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let border = cx.theme().border;
        let sql_mode = self.active_tab().mode == TabMode::Sql;
        let blocked =
            self.active_tab().changed || self.reloading == Some(self.active_tab().source.id);
        div()
            .key_context("RowglideBrowse")
            .px_5()
            .py_2()
            .flex()
            .items_center()
            .gap_2()
            .border_b_1()
            .border_color(border)
            .flex_shrink_0()
            .child(
                div().flex_1().min_w(px(160.)).max_w(px(320.)).child(
                    Input::new(&self.search)
                        .id("browse-filter")
                        .aria_label(tr!("a11y.filter"))
                        .small()
                        .disabled(sql_mode || blocked),
                ),
            )
            .child(div().text_xs().child(tr!("browse.limit")))
            .child(
                div().w(px(80.)).flex_shrink_0().child(
                    Input::new(&self.limit)
                        .id("browse-limit")
                        .aria_label(tr!("browse.limit"))
                        .small()
                        .disabled(sql_mode || blocked),
                ),
            )
            .child(div().text_xs().child(tr!("browse.offset")))
            .child(
                div().w(px(80.)).flex_shrink_0().child(
                    Input::new(&self.offset)
                        .id("browse-offset")
                        .aria_label(tr!("browse.offset"))
                        .small()
                        .disabled(sql_mode || blocked),
                ),
            )
            .child(
                Button::new("filter")
                    .small()
                    .label(tr!("common.apply"))
                    .disabled(sql_mode || blocked)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.apply_filter(window, cx);
                    })),
            )
            .child(
                Button::new("reset")
                    .small()
                    .label(tr!("common.reset"))
                    .disabled(sql_mode || blocked)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.query = Query {
                            source: this.query.source,
                            schema_revision: this.active_tab().source.schema_revision,
                            limit: Some(BrowsePage::default().limit()),
                            ..Default::default()
                        };

                        this.active_tab_mut().drafts = super::state::BrowseDraft::default();
                        this.reset_inputs(window, cx);

                        this.run(window, cx);
                    })),
            )
            .child(div().flex_1())
            .when(self.active_tab().source.parquet.is_some(), |toolbar| {
                toolbar.child(
                    Button::new("file-info-toggle")
                        .disabled(blocked)
                        .small()
                        .label(if self.active_tab().info.open {
                            tr!("info.hide")
                        } else {
                            tr!("info.open")
                        })
                        .tooltip(tr!("info.tooltip"))
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_file_info(cx))),
                )
            })
            .child(
                Button::new("sql-toggle")
                    .small()
                    .label(if self.sql_open {
                        tr!("common.browse")
                    } else {
                        SharedString::new_static("SQL")
                    })
                    .on_click(cx.listener(|this, _, window, cx| this.switch_mode(window, cx))),
            )
    }

    /// Build the SQL editor panel and explicit Run query action.
    ///
    /// The hint names the `current_file` relation and accurately describes the current
    /// continuous result loading. Browse pagination controls do not govern SQL.
    pub(super) fn render_sql(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .p_4()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().muted)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .mb_3()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("SQL · current_file"),
                    )
                    .child(
                        Button::new("run")
                            .disabled(self.active_tab().changed)
                            .small()
                            .primary()
                            .label(tr!("sql.run"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.run_sql(window, cx);
                            })),
                    ),
            )
            .child(
                div().key_context("RowglideSql").child(
                    Editor::new(&self.sql)
                        .aria_label(tr!("a11y.sql_editor"))
                        .h(px(95.)),
                ),
            )
            .child(
                div()
                    .mt_2()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr!("sql.help")),
            )
    }

    /// Build the compact status bar with mode, errors, reload/cancel and navigation.
    ///
    /// Browse-only Previous/Next buttons use the applied page and reject pending drafts
    /// or busy queries. Missing cached results can be reloaded explicitly. Status text
    /// reports actual retained ranges rather than an uncomputed source total.
    pub(super) fn render_status(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let border = cx.theme().status_bar_border;
        div()
            .min_h(px(28.))
            .px_5()
            .py(px(2.))
            .border_t_1()
            .border_color(border)
            .bg(cx.theme().status_bar)
            .flex()
            .items_center()
            .gap_3()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .text_xs()
                    .text_color(if self.error {
                        cx.theme().red
                    } else {
                        cx.theme().muted_foreground
                    })
                    .child(format!(
                        "{} · {}",
                        if self.sql_open {
                            SharedString::new_static("SQL")
                        } else {
                            tr!("common.browse")
                        },
                        self.status
                    )),
            )
            .when_some(self.clipboard.message.clone(), |s, message| {
                s.child(
                    div()
                        .max_w(px(360.))
                        .overflow_hidden()
                        .text_ellipsis()
                        .text_xs()
                        .child(message),
                )
            })
            .when(
                !self.busy
                    && !self.active_tab().changed
                    && !self.table.read(cx).delegate().has_result()
                    && (!self.sql_open || self.active_tab().last_sql.is_some()),
                |s| {
                    s.child(
                        Button::new("reload-result")
                            .small()
                            .ghost()
                            .label(tr!("query.reload"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.query = this.active_tab().active_query();
                                this.run(window, cx);
                            })),
                    )
                },
            )
            .when(self.busy, |s| {
                s.child(
                    Button::new("cancel")
                        .small()
                        .ghost()
                        .label(tr!("common.cancel"))
                        .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                )
            })
            // Browse navigation replaces the current page; it never appends rows.
            .when(!self.sql_open, |s| {
                s.child(
                    Button::new("previous-page")
                        .small()
                        .ghost()
                        .label(tr!("common.previous"))
                        .disabled(!self.can_turn_page(false, cx))
                        .on_click(
                            cx.listener(|this, _, window, cx| this.turn_page(false, window, cx)),
                        ),
                )
                .child(
                    Button::new("next-page")
                        .small()
                        .ghost()
                        .label(tr!("common.next"))
                        .disabled(!self.can_turn_page(true, cx))
                        .on_click(
                            cx.listener(|this, _, window, cx| this.turn_page(true, window, cx)),
                        ),
                )
            })
            .when_some(
                super::file_info::source_counts(&self.active_tab().source),
                |bar, label| {
                    bar.child(
                        div()
                            .flex_shrink_0()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(label),
                    )
                },
            )
            .child(self.render_memory(cx))
    }
}
