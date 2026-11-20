//! Empty-workspace welcome page and asynchronous recent-file history loading.

use super::Workspace;
use crate::i18n::tr;
use gpui_kit::component::{
    ActiveTheme, Icon, IconName,
    button::{Button, ButtonVariants},
};
use gpui_kit::*;

impl Workspace {
    /// Read optional preferences in the background without delaying the initial window.
    /// A startup open can finish first; its newer history must win over this snapshot.
    pub(super) fn load_recent(&mut self, cx: &mut Context<Self>) {
        let Some(store) = self.recent_store.clone() else {
            return;
        };
        let revision = self.recent_revision;
        cx.spawn(async move |this, cx| {
            let entries = cx
                .background_executor()
                .spawn(async move { store.entries() })
                .await;
            match entries {
                Ok(entries) => {
                    let _ = this.update(cx, |this, cx| {
                        if this.recent_revision == revision {
                            this.recent_files = entries;
                            cx.notify();
                        }
                    });
                }
                Err(error) => eprintln!("{error}"),
            }
        })
        .detach();
    }

    /// Render a centered welcome page with a primary opener and up to five recent paths.
    /// Each button displays the parent directory and exposes the full path to screen readers.
    /// Clicking uses the same cancellable background workflow as picker and dropped files.
    pub(super) fn render_welcome(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("welcome")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .items_center()
            .p_8()
            .child(
                div()
                    .w_full()
                    .max_w(px(640.))
                    .my_auto()
                    .flex_shrink_0()
                    .flex()
                    .flex_col()
                    .gap_6()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .mb_4()
                            .child(
                                div()
                                    .text_3xl()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Rowglide"),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(tr!("welcome.subtitle")),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(self.welcome_heading(tr!("welcome.start"), cx))
                            .child(
                                Button::new("open-files")
                                    .track_focus(&self.open_focus)
                                    .ghost()
                                    .w_full()
                                    .h(px(44.))
                                    .justify_start()
                                    .icon(IconName::FolderOpen)
                                    .label(tr!("files.open"))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.open_files(window, cx)
                                    })),
                            )
                            .child(
                                div()
                                    .px_3()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(tr!("welcome.drop")),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(self.welcome_heading(tr!("welcome.recent"), cx))
                            .children(self.recent_files.iter().take(5).enumerate().map(
                                |(index, path)| {
                                    let name = path
                                        .file_name()
                                        .unwrap_or(path.as_os_str())
                                        .to_string_lossy()
                                        .into_owned();
                                    let parent = path
                                        .parent()
                                        .map(|parent| parent.display().to_string())
                                        .unwrap_or_default();
                                    let full_path = path.display().to_string();
                                    let path = path.clone();
                                    Button::new(("recent-file", index))
                                        .ghost()
                                        .w_full()
                                        .h(px(56.))
                                        .justify_start()
                                        .accessibility_label(tr!(
                                            "welcome.open_recent",
                                            path = full_path
                                        ))
                                        .child(Icon::new(IconName::File))
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .flex()
                                                .flex_col()
                                                .items_start()
                                                .gap_1()
                                                .child(
                                                    div()
                                                        .w_full()
                                                        .overflow_hidden()
                                                        .text_ellipsis()
                                                        .child(name),
                                                )
                                                .child(
                                                    div()
                                                        .w_full()
                                                        .overflow_hidden()
                                                        .text_ellipsis_middle()
                                                        .text_xs()
                                                        .text_color(cx.theme().muted_foreground)
                                                        .child(parent),
                                                ),
                                        )
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.open_paths(vec![path.clone()], window, cx)
                                        }))
                                },
                            ))
                            .children(self.recent_files.is_empty().then(|| {
                                div()
                                    .px_3()
                                    .py_2()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(tr!("welcome.no_recent"))
                            })),
                    ),
            )
    }

    /// Pair a muted section label with a flexible divider using semantic theme colors.
    fn welcome_heading(&self, label: SharedString, cx: &App) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .gap_3()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(label)
            .child(div().flex_1().h(px(1.)).bg(cx.theme().border))
    }
}
