//! Compact file summary and paged metadata presentation beside the data table.

use super::{Workspace, table::limited_text, value_text};
use gpui_kit::component::{
    ActiveTheme, Disableable, Sizable,
    button::{Button, ButtonVariants},
    tab::{Tab, TabBar},
    table::DataTable,
};
use gpui_kit::*;
use rowglide_core::ParquetSection;

impl Workspace {
    /// Render the fixed-width side panel with header, compact section tabs and bounded content.
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
        let mut panel = div()
            .w(px(480.))
            .flex_shrink_0()
            .h_full()
            .min_h_0()
            .border_l_1()
            .border_color(rgb(0xe4e8e7))
            .bg(rgb(0xfafbfa))
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
                            .child("Parquet file info"),
                    )
                    .child(
                        Button::new("close-file-info")
                            .small()
                            .ghost()
                            .label("Close")
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
                        ["Overview", "Schema", "Row groups", "Metadata"]
                            .into_iter()
                            .map(|name| Tab::new().label(name).px_2()),
                    )
                    .on_click(cx.listener(|this, index: &usize, _, cx| {
                        this.select_info_section(*index, cx)
                    })),
            );
        if section.is_none() {
            return panel.child(self.render_file_overview()).into_any_element();
        }
        let hint = match section {
            Some(ParquetSection::Schema) => {
                "Original schema order, including group nodes. Child counts describe nesting."
            }
            Some(ParquetSection::RowGroups) => {
                "One row per column chunk. null means not recorded; check min/max exactness flags."
            }
            _ => "Original key/value bytes. Valid UTF-8 is shown as text; other bytes as 0x hex.",
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
                            .text_color(rgb(0xb34c46))
                            .child(error.clone()),
                    )
                    .child(
                        Button::new("retry-file-info")
                            .small()
                            .label("Retry")
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
                    .child(div().text_sm().child("Loading metadata…"))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("May wait for the current data query."),
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
                    .border_color(rgb(0xe4e8e7))
                    .px_4()
                    .py_2()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(div().text_xs().child("Selected value · preview"))
                            .child(
                                Button::new("copy-metadata-value")
                                    .small()
                                    .ghost()
                                    .label("Copy full value")
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
            "Loading…".into()
        } else if self.file_info.error.is_some() {
            "Unavailable".into()
        } else if rows == 0 {
            "0 entries".into()
        } else {
            format!("Entries {}–{}", offset + 1, offset + rows)
        };
        panel
            .child(
                div()
                    .px_3()
                    .py_1()
                    .border_t_1()
                    .border_color(rgb(0xe4e8e7))
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_1().text_xs().child(label))
                    .child(
                        Button::new("metadata-previous")
                            .small()
                            .ghost()
                            .label("Previous")
                            .disabled(grid.loading || offset == 0)
                            .on_click(cx.listener(|this, _, _, cx| this.turn_info_page(false, cx))),
                    )
                    .child(
                        Button::new("metadata-next")
                            .small()
                            .ghost()
                            .label("Next")
                            .disabled(
                                grid.loading
                                    || !grid.page.as_ref().is_some_and(|page| page.has_next),
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.turn_info_page(true, cx))),
                    ),
            )
            .into_any_element()
    }

    /// Present opening-time source facts without changing query results or inspecting disk.
    fn render_file_overview(&self) -> impl IntoElement {
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
                ("Format", "Parquet".to_string()),
                ("File size", format_file_size(summary.file_bytes)),
                (
                    "Rows",
                    source
                        .rows
                        .map_or("Unknown".into(), |rows| rows.to_string()),
                ),
                ("Columns", format!("{} top-level fields", summary.columns)),
                ("Row groups", summary.row_groups.to_string()),
                ("Format version", summary.format_version.to_string()),
                (
                    "Created by",
                    limited_text(
                        summary.created_by.as_deref().unwrap_or("Not recorded"),
                        Some(512),
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
                                .text_color(rgb(0x6c7b73))
                                .child(label),
                        )
                        .child(div().flex_1().min_w_0().child(value)),
                );
            }
        }
        content.child(div().mt_2().text_xs().text_color(rgb(0x6c7b73)).child("Source facts captured when opened. Filters and SQL do not change these counts. Keep the file unchanged while it is open. Compression and statistics are listed per column chunk under Row groups."))
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
