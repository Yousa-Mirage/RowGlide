//! Selected-cell details and expandable nested values.

use super::Workspace;
use crate::cells;
use arrow_array::ArrayRef;
use gpui_kit::component::{
    ActiveTheme, Sizable,
    button::{Button, ButtonVariants},
};
use gpui_kit::*;

impl Workspace {
    /// Build the fixed-width panel for one selected Arrow cell and its nested children.
    ///
    /// `name` labels the source column; `row` indexes the supplied shared array. The
    /// array reference keeps data alive while the panel is visible. A close action
    /// clears the selected cell; nested expansion state is managed by `Self::tree`.
    pub(super) fn render_inspector(
        &self,
        name: String,
        array: ArrayRef,
        row: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let border = rgb(0xe4e8e7);
        div()
            .w(px(272.))
            .flex_shrink_0()
            .h_full()
            .border_l_1()
            .border_color(border)
            .flex()
            .flex_col()
            .child(
                div()
                    .p_4()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Cell inspector"),
                    )
                    .child(
                        Button::new("close-inspector")
                            .small()
                            .ghost()
                            .label("×")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.selected = None;
                                cx.notify();
                            })),
                    ),
            )
            .child(
                div()
                    .px_4()
                    .pb_3()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!(
                        "{} · {}",
                        name,
                        cells::type_label(array.data_type())
                    )),
            )
            .child(
                div()
                    .id("inspector-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(self.tree(name, array, row, "value".into(), 0, cx)),
            )
    }

    /// Render one expandable inspector node and its currently expanded descendants.
    ///
    /// `path` identifies expansion state, `depth` controls indentation and recursion
    /// stops below depth 12. Children retain typed Arrow references and list/map child
    /// counts are capped by `cells::children`. This is a bounded recursive tree, not a
    /// virtualized or complete export of arbitrarily large nested values.
    fn tree(
        &self,
        label: String,
        array: ArrayRef,
        row: usize,
        path: String,
        depth: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let children = cells::children(&array, row);
        let open = self.expanded.contains(&path);
        let has_children = !children.is_empty() && depth < 12;
        let click_path = path.clone();
        let mut node = div().flex().flex_col().child(
            div()
                .id(SharedString::from(path.clone()))
                .px_3()
                .py_2()
                .ml(px(depth as f32 * 12.))
                .rounded_md()
                .hover(|s| s.bg(rgb(0xf0f3f2)))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !this.expanded.remove(&click_path) {
                        this.expanded.insert(click_path.clone());
                    }
                    cx.notify();
                }))
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!(
                            "{} {}",
                            if has_children {
                                if open { "▾" } else { "▸" }
                            } else {
                                "·"
                            },
                            label
                        )),
                )
                .child(
                    div()
                        .text_sm()
                        .overflow_hidden()
                        .child(cells::summary(&array, row)),
                ),
        );

        if open && has_children {
            for (i, (label, child, row)) in children.into_iter().enumerate() {
                node =
                    node.child(self.tree(label, child, row, format!("{path}/{i}"), depth + 1, cx));
            }
        }

        node.into_any_element()
    }
}
