//! File navigation, browse controls, SQL editor, and status bar.

use super::{TabMode, Workspace};
use gpui_kit::component::{
    ActiveTheme, Disableable, Sizable,
    button::{Button, ButtonVariants},
    input::{Editor, Input},
    tab::{Tab, TabBar},
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use rowglide_app::Query;

impl Workspace {
    // File tabs restore the selected file session.
    pub(super) fn render_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        TabBar::new("open-file-tabs")
            .folder()
            .selected_index(self.active_tab)
            .children(
                self.tabs
                    .iter()
                    .map(|tab| Tab::new().label(tab.source.name.clone())),
            )
            .on_click(
                cx.listener(|this, index: &usize, window, cx| this.switch_tab(*index, window, cx)),
            )
    }

    // Compact browse controls and SQL panel toggle.
    pub(super) fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let border = rgb(0xe4e8e7);
        let sql_mode = self.active_tab().mode == TabMode::Sql;
        div()
            .px_5()
            .py_2()
            .flex()
            .items_center()
            .gap_2()
            .border_b_1()
            .border_color(border)
            .flex_shrink_0()
            .child(
                div()
                    .flex_1()
                    .min_w(px(160.))
                    .max_w(px(320.))
                    .child(Input::new(&self.search).small().disabled(sql_mode)),
            )
            .child(div().text_xs().child("Limit"))
            .child(
                div()
                    .w(px(80.))
                    .flex_shrink_0()
                    .child(Input::new(&self.limit).small().disabled(sql_mode)),
            )
            .child(div().text_xs().child("Offset"))
            .child(
                div()
                    .w(px(80.))
                    .flex_shrink_0()
                    .child(Input::new(&self.offset).small().disabled(sql_mode)),
            )
            .child(
                Button::new("filter")
                    .small()
                    .label("Apply")
                    .disabled(sql_mode)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.apply_filter(window, cx);
                    })),
            )
            .child(
                Button::new("reset")
                    .small()
                    .ghost()
                    .label("Reset")
                    .disabled(sql_mode)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.query = Query {
                            source: this.query.source,
                            ..Default::default()
                        };
                        this.active_tab_mut().browse = this.query.clone();
                        this.reset_inputs(window, cx);

                        this.run(window, cx);
                    })),
            )
            .child(div().flex_1())
            .child(
                Button::new("sql-toggle")
                    .small()
                    .label(if self.sql_open { "Browse" } else { "SQL" })
                    .on_click(cx.listener(|this, _, window, cx| this.switch_mode(window, cx))),
            )
    }

    // Demo SQL editor; execution remains coordinated by Workspace.
    pub(super) fn render_sql(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div().p_4().border_b_1().border_color(rgb(0xe4e8e7)).bg(rgb(0xf9faf9))
            .child(
                div().flex().items_center().justify_between().mb_3()
                    .child(div().text_xs().text_color(cx.theme().muted_foreground).child("SQL · DEMO PRESETS ONLY"))
                    .child(Button::new("run").small().primary().label("Run query")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.run_sql(window, cx);
                        }))),
            )
            .child(Editor::new(&self.sql).h(px(95.)))
            .child(div().mt_2().text_xs().text_color(cx.theme().muted_foreground)
                .child("Supported: SELECT * FROM current_file LIMIT 100;   /   SELECT * FROM current_file WHERE false;"))
    }

    // Bottom status bar: query status, cancellation, and sample-data label.
    pub(super) fn render_status(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let border = rgb(0xe4e8e7);
        div()
            .min_h(px(28.))
            .px_5()
            .py(px(2.))
            .border_t_1()
            .border_color(border)
            .flex()
            .items_center()
            .gap_3()
            .child(
                div()
                    .flex_1()
                    .text_xs()
                    .text_color(if self.error {
                        rgb(0xb34c46)
                    } else {
                        rgb(0x6c7b73)
                    })
                    .child(format!(
                        "{} · {}",
                        if self.sql_open { "SQL" } else { "Browse" },
                        self.status
                    )),
            )
            .when(
                !self.busy
                    && self.table.read(cx).delegate().preview.is_none()
                    && (!self.sql_open || self.active_tab().last_sql.is_some()),
                |s| {
                    s.child(
                        Button::new("reload-result")
                            .small()
                            .ghost()
                            .label("Reload result")
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
                        .label("Cancel")
                        .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                )
            })
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(0x6c7b73))
                    .child("SAMPLE DATA"),
            )
    }
}
