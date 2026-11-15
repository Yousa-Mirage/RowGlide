//! Selected-cell details and expandable nested values.

use super::Workspace;
use crate::cells;
use crate::clipboard::{CopyTarget, copy_menu};
use arrow_array::ArrayRef;
use gpui_kit::component::{
    ActiveTheme, IconName, Sizable,
    button::{Button, ButtonVariants},
    menu::ContextMenuExt,
    resizable::{HandleEdge, resize_handle},
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

/// Identifies inspector-divider drags without attaching a visible drag preview.
#[derive(Clone)]
pub(super) struct ResizeInspector;

impl Render for ResizeInspector {
    /// The divider itself provides feedback; no floating preview follows the pointer.
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

impl Workspace {
    /// Build a resizable panel headed by the selected column's name and Arrow type.
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
            .relative()
            .w(self.inspector_width)
            .max_w(relative(0.65))
            .flex_shrink_0()
            .h_full()
            .min_h_0()
            .border_l_1()
            .border_color(border)
            .flex()
            .flex_col()
            .child(
                div()
                    .px_3()
                    .py_2()
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(format!(
                                "{} · {}",
                                name,
                                cells::type_label(array.data_type())
                            )),
                    )
                    .child(
                        Button::new("close-inspector")
                            .small()
                            .ghost()
                            .icon(IconName::Close)
                            .tooltip("Close cell inspector")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.selected = None;
                                cx.notify();
                            })),
                    ),
            )
            .child(
                div()
                    .id("inspector-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(self.tree(name, array, row, "value".into(), 0, cx)),
            )
            .child(
                resize_handle("inspector-divider", Axis::Horizontal)
                    .inside(HandleEdge::Leading)
                    .on_drag(ResizeInspector, |drag, _, _, cx| cx.new(|_| drag.clone())),
            )
    }

    /// Render one expandable inspector node and its currently expanded descendants.
    ///
    /// `path` identifies expansion state, `depth` controls indentation and recursion
    /// stops below depth 12. Children retain typed Arrow references and list/map child
    /// counts are capped by `cells::children`. This is a bounded recursive tree, not a
    /// virtualized or complete export of arbitrarily large nested values.
    /// Root scalars omit the repeated field label; child names and expandable roots
    /// retain their labels and disclosure indicators.
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
        let target = CopyTarget::Cell(array.clone(), row);
        let events = self.table.read(cx).delegate().events.clone();
        let mut node = div().flex().flex_col().child(
            div()
                .id(SharedString::from(path.clone()))
                .px_3()
                .py_2()
                .ml(px(depth as f32 * 12.))
                .rounded_md()
                .hover(|s| s.bg(rgb(0xf0f3f2)))
                .when(has_children, |node| node.cursor_pointer())
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !has_children {
                        return;
                    }
                    if !this.expanded.remove(&click_path) {
                        this.expanded.insert(click_path.clone());
                    }
                    cx.notify();
                }))
                .when(depth > 0 || has_children, |node| {
                    node.child(
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
                })
                .child(
                    div()
                        .text_sm()
                        .overflow_hidden()
                        .child(cells::summary(&array, row)),
                )
                .context_menu(move |menu, _, _| {
                    copy_menu(menu, Some(target.clone()), events.clone())
                }),
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
