//! File navigation, browse controls, SQL editor, and status bar.

use super::Workspace;
use gpui_kit::component::{
    ActiveTheme, Sizable,
    button::{Button, ButtonVariants},
    input::{Editor, Input},
    tab::{Tab, TabBar},
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use rowglide_app::Query;

impl Workspace {
    // File tabs reset browse controls when switching datasets.
    pub(super) fn render_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        TabBar::new("open-file-tabs")
            .folder()
            .selected_index(
                self.sources
                    .iter()
                    .position(|source| source.id == self.query.source)
                    .unwrap_or(0),
            )
            .children(
                self.sources
                    .iter()
                    .map(|source| Tab::new().label(source.name.clone())),
            )
            .on_click(cx.listener(|this, index: &usize, window, cx| {
                let Some(source) = this.sources.get(*index) else {
                    return;
                };
                if source.id == this.query.source {
                    return;
                }
                this.query = Query {
                    source: source.id,
                    ..Default::default()
                };
                this.reset_inputs(window, cx);

                this.run(window, cx);
            }))
    }

    // Compact browse controls and SQL panel toggle.
    pub(super) fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let border = rgb(0xe4e8e7);
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
                    .child(Input::new(&self.search).small()),
            )
            .child(div().text_xs().child("Limit"))
            .child(
                div()
                    .w(px(80.))
                    .flex_shrink_0()
                    .child(Input::new(&self.limit).small()),
            )
            .child(div().text_xs().child("Offset"))
            .child(
                div()
                    .w(px(80.))
                    .flex_shrink_0()
                    .child(Input::new(&self.offset).small()),
            )
            .child(
                Button::new("filter")
                    .small()
                    .label("Apply")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.apply_filter(window, cx);
                    })),
            )
            .child(
                Button::new("reset")
                    .small()
                    .ghost()
                    .label("Reset")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.query = Query {
                            source: this.query.source,
                            ..Default::default()
                        };
                        this.reset_inputs(window, cx);

                        this.run(window, cx);
                    })),
            )
            .child(div().flex_1())
            .child(
                Button::new("sql-toggle")
                    .small()
                    .label(if self.sql_open { "Hide SQL" } else { "SQL" })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.sql_open = !this.sql_open;
                        cx.notify();
                    })),
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
                            this.query.search.clear();
                            this.search.update(cx, |state, cx| state.set_value("", window, cx));
                            this.query.sort = None;
                            this.query.sql = Some(this.sql.read(cx).value().to_string());
                            this.run(window, cx);
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
                    .child(self.status.clone()),
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
