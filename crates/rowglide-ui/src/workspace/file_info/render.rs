//! Compact file summary and paged metadata presentation beside the data table.

use crate::i18n::tr;

use super::{Workspace, table::limited_text, value_text};
use crate::workspace::ResizePanel;
use gpui_kit::component::{
    ActiveTheme, Disableable, Sizable,
    button::{Button, ButtonVariants},
    resizable::{HandleEdge, resize_handle},
    tab::{Tab, TabBar},
    table::DataTable,
};
use gpui_kit::*;
use rowglide_core::ParquetSection;

impl Workspace {
    /// Render the resizable side panel with header, compact section tabs and bounded content.
    pub(in crate::workspace) fn render_file_info(
        &self,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let section = self.active_tab().info.section;
        let index = match section {
            None => 0,
            Some(ParquetSection::Schema) => 1,
            Some(ParquetSection::RowGroups) => 2,
            Some(ParquetSection::KeyValue) => 3,
        };
        let handle = resize_handle("file-info-divider", Axis::Horizontal)
            .inside(HandleEdge::Leading)
            .on_drag(ResizePanel::FileInfo, |drag, _, _, cx| {
                cx.new(|_| drag.clone())
            });
        let mut panel = div()
            .relative()
            .w(self.file_info_width)
            .max_w(relative(0.65))
            .flex_shrink_0()
            .h_full()
            .min_h_0()
            .border_l_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().muted)
            .flex()
            .flex_col()
            .child(
                div()
                    .px_4()
                    .py_2()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(tr!("info.title")),
                    )
                    .child(
                        Button::new("close-file-info")
                            .small()
                            .ghost()
                            .label(tr!("common.close"))
                            .on_click(cx.listener(|this, _, _, cx| this.close_file_info(cx))),
                    ),
            )
            .child(
                div()
                    .px_4()
                    .pb_2()
                    .text_xs()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(self.active_tab().source.name.clone()),
            )
            .child(
                TabBar::new("file-info-sections")
                    .small()
                    .underline()
                    .px_3()
                    .selected_index(index)
                    .children(
                        [
                            tr!("info.overview"),
                            tr!("info.schema"),
                            tr!("info.row_groups"),
                            tr!("info.metadata"),
                        ]
                        .into_iter()
                        .map(|name| Tab::new().label(name).px_2()),
                    )
                    .on_click(cx.listener(|this, index: &usize, _, cx| {
                        this.select_info_section(*index, cx)
                    })),
            );
        if section.is_none() {
            return panel
                .child(self.render_file_overview(cx))
                .child(handle)
                .into_any_element();
        }
        let hint = match section {
            Some(ParquetSection::Schema) => {
                tr!("info.schema_hint")
            }
            Some(ParquetSection::RowGroups) => {
                tr!("info.groups_hint")
            }
            _ => tr!("info.metadata_hint"),
        };
        panel = panel.child(
            div()
                .px_4()
                .py_2()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(hint),
        );
        let grid = self.file_info.table.read(cx).delegate();
        if let Some(error) = &self.file_info.error {
            panel = panel.child(
                div()
                    .id("file-info-error")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().red)
                            .child(error.clone()),
                    )
                    .child(
                        Button::new("retry-file-info")
                            .small()
                            .label(tr!("common.retry"))
                            .on_click(cx.listener(|this, _, _, cx| this.refresh_file_info(cx))),
                    ),
            );
        } else if grid.loading {
            panel = panel.child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_3()
                    .child(div().text_sm().child(tr!("info.loading")))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(tr!("info.wait")),
                    ),
            );
        } else {
            panel = panel.child(
                // Keep wide metadata inside the panel's own scrollable table viewport.
                div()
                    .flex_1()
                    .w_full()
                    .min_h_0()
                    .min_w_0()
                    .overflow_hidden()
                    .child(
                        DataTable::new(&self.file_info.table)
                            .scroll_bounce(false)
                            .small()
                            .stripe(false)
                            .bordered(false),
                    ),
            );
        }
        if let Some((row, col)) = self.file_info.selected
            && let Some((array, local)) = grid.cell(row, col)
        {
            let text =
                value_text(array, local, Some(2000)).unwrap_or_else(|error| error.to_string());
            panel = panel.child(
                div()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .px_4()
                    .py_2()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(div().text_xs().child(tr!("info.preview")))
                            .child(
                                Button::new("copy-metadata-value")
                                    .small()
                                    .ghost()
                                    .label(tr!("info.copy"))
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.copy_info_value(cx)),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .id("metadata-value-scroll")
                            .h(px(80.))
                            .overflow_scroll()
                            .text_xs()
                            .child(text),
                    ),
            );
        }
        let offset = self.active_tab().info.offset;
        let rows = grid.page.as_ref().map_or(0, |page| page.preview.rows);
        let label = if grid.loading {
            tr!("common.loading")
        } else if self.file_info.error.is_some() {
            tr!("common.unavailable")
        } else if rows == 0 {
            tr!("info.empty")
        } else {
            tr!("info.entries", start = offset + 1, end = offset + rows)
        };
        panel
            .child(
                div()
                    .px_3()
                    .py_1()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_1().text_xs().child(label))
                    .child(
                        Button::new("metadata-previous")
                            .small()
                            .ghost()
                            .label(tr!("common.previous"))
                            .disabled(grid.loading || offset == 0)
                            .on_click(cx.listener(|this, _, _, cx| this.turn_info_page(false, cx))),
                    )
                    .child(
                        Button::new("metadata-next")
                            .small()
                            .ghost()
                            .label(tr!("common.next"))
                            .disabled(
                                grid.loading
                                    || !grid.page.as_ref().is_some_and(|page| page.has_next),
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.turn_info_page(true, cx))),
                    ),
            )
            .child(handle)
            .into_any_element()
    }

    /// Present opening-time source facts without changing query results or inspecting disk.
    /// `cx` supplies the active theme's muted label colors.
    fn render_file_overview(&self, cx: &App) -> impl IntoElement {
        let source = &self.active_tab().source;
        let mut content = div()
            .id("file-overview-scroll")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p_4()
            .flex()
            .flex_col()
            .gap_4();
        if let Some(summary) = &source.parquet {
            for (label, value) in [
                (tr!("info.format"), SharedString::new_static("Parquet")),
                (
                    tr!("info.size"),
                    format_file_size(summary.file_bytes).into(),
                ),
                (
                    tr!("info.rows"),
                    source
                        .rows
                        .map_or(tr!("info.unknown"), |rows| rows.to_string().into()),
                ),
                (
                    tr!("info.columns"),
                    tr!("info.fields", count = summary.columns),
                ),
                (
                    tr!("info.row_groups"),
                    summary.row_groups.to_string().into(),
                ),
                (
                    tr!("info.version"),
                    summary.format_version.to_string().into(),
                ),
                (
                    tr!("info.creator"),
                    summary.created_by.as_deref().map_or_else(
                        || tr!("info.not_recorded"),
                        |creator| limited_text(creator, Some(512)).into(),
                    ),
                ),
            ] {
                content = content.child(
                    div()
                        .flex()
                        .gap_3()
                        .text_sm()
                        .child(
                            div()
                                .w(px(110.))
                                .flex_shrink_0()
                                .text_color(cx.theme().muted_foreground)
                                .child(label),
                        )
                        .child(div().flex_1().min_w_0().child(value)),
                );
            }
        }
        content.child(
            div()
                .mt_2()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(tr!("info.note")),
        )
    }
}

/// Format source bytes with one decimal using decimal KB, MB or GB.
/// Choose the unit before rounding: below 1 MB uses KB, below 1 GB uses MB,
/// and larger files use GB. Even files smaller than 1 KB remain expressed in KB.
fn format_file_size(bytes: u64) -> String {
    let (divisor, unit) = if bytes < 1_000_000 {
        (1_000., "KB")
    } else if bytes < 1_000_000_000 {
        (1_000_000., "MB")
    } else {
        (1_000_000_000., "GB")
    };
    format!("{:.1} {unit}", bytes as f64 / divisor)
}
