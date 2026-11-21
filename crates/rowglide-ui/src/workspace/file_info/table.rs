//! Virtualized footer-detail table and lossless formatting for selected values.

use crate::{cells, i18n::tr};

use arrow_schema::DataType;
use gpui_kit::component::{
    ActiveTheme,
    table::{Column, TableDelegate, TableState},
};
use gpui_kit::*;
use rowglide_core::SchemaRow;

/// Retains the complete logical schema, with original field types and nullability.
#[derive(Default)]
pub(super) struct MetadataGrid {
    /// Logical fields in display order, cleared when switching source or section.
    pub(super) rows: Vec<SchemaRow>,
    /// Loading is separate from the main data table's query lifecycle.
    pub(super) loading: bool,
}

impl MetadataGrid {
    /// Measure the fixed row-number gutter from the largest row on the schema.
    fn gutter_width(&self) -> Pixels {
        let rows = self.rows_count_without_app().max(1);
        let digits = rows.ilog10() + 1;
        px(20. + 9. * digits.max(2) as f32)
    }

    /// Read the schema row count without requiring a GPUI application handle.
    fn rows_count_without_app(&self) -> usize {
        self.rows.len()
    }

    /// Render the fixed gutter used by both the schema header and its body rows.
    fn number_gutter(&self, label: SharedString, cx: &App) -> Div {
        div()
            .w(self.gutter_width())
            .h_full()
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_end()
            .px_2()
            .bg(cx.theme().background)
            .border_r_1()
            .border_color(cx.theme().table_row_border)
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(label)
    }

    /// Measure up to 100 field previews and the header using the UI font.
    /// Widths are computed on table refresh, not on each rendered cell. Multiline
    /// values use their widest line; the column separately caps excessive widths.
    fn content_width(&self, column: usize, cx: &App) -> Pixels {
        let header = header(column);
        let labels = std::iter::once(std::borrow::Cow::Borrowed(header.as_str())).chain(
            (0..self.rows.len().min(100))
                .map(|row| std::borrow::Cow::Owned(self.label(row, column))),
        );
        let text_system = WindowTextSystem::new(cx.text_system().clone());
        let mut width = px(0.);
        for label in labels {
            for line in label.lines() {
                let run = TextRun {
                    len: line.len(),
                    font: font(cx.theme().font_family.clone()),
                    color: cx.theme().foreground,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                // Use the header's 14 px font as a conservative bound for 12 px cells.
                width = width.max(
                    text_system
                        .shape_line(line.to_owned().into(), px(14.), &[run], None)
                        .width,
                );
            }
        }
        width
    }

    /// Return the displayed value without indentation for display and accessibility.
    fn value(&self, row: usize, column: usize) -> Option<String> {
        let field = &self.rows.get(row)?.field;
        Some(match column {
            0 => field.name().clone(),
            1 => detailed_type(field.data_type()),
            2 if field.is_nullable() => tr!("metadata.yes").to_string(),
            2 => tr!("metadata.no").to_string(),
            _ => return None,
        })
    }

    /// Indent actual child names and bound the text shown inside the table.
    fn label(&self, row: usize, column: usize) -> String {
        let value = self.value(row, column).unwrap_or_default();
        let mut text = limited_text(&value, Some(160));
        if column == 0
            && let Some(field) = self.rows.get(row)
        {
            text.insert_str(0, &"    ".repeat(field.depth));
        }
        text
    }
}

impl TableDelegate for MetadataGrid {
    /// Expose the page schema even when the metadata relation contains no records.
    fn columns_count(&self, _: &App) -> usize {
        3
    }

    /// Expose only retained metadata records, never source-data rows.
    fn rows_count(&self, _: &App) -> usize {
        self.rows.len()
    }

    /// Keep row numbers fixed while the three schema columns scroll or resize.
    fn render_header(
        &mut self,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        div()
            .id("metadata-header")
            .child(self.number_gutter("#".into(), cx))
    }

    /// Add a fixed row number before the logical schema fields.
    fn render_tr(
        &mut self,
        row: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        let label = if row < self.rows_count(cx) {
            (row + 1).to_string().into()
        } else {
            SharedString::default()
        };
        div()
            .id(("metadata-row", row))
            .child(self.number_gutter(label, cx))
    }

    /// Fit headers and sampled values within readable width bounds.
    /// Wide relations remain horizontally scrollable; backend order defines pagination.
    fn column(&self, index: usize, cx: &App) -> Column {
        Column::new(format!("metadata-{index}"), header(index))
            .width((self.content_width(index, cx) + px(24.)).clamp(px(64.), px(320.)))
            .min_width(px(64.))
            .max_width(px(640.))
            .movable(false)
    }

    /// Refit a manually resized column on double-click, including the first 100 fields.
    /// The table adds cell padding and applies the column's minimum and maximum width.
    fn fit_column(
        &mut self,
        column: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> Option<Pixels> {
        Some(self.content_width(column, cx))
    }

    /// Show the component's loading indicator during a detail request.
    fn loading(&self, _: &App) -> bool {
        self.loading
    }

    /// Format only virtualized visible cells; metadata does not trigger queries during rendering.
    fn render_td(
        &mut self,
        row: usize,
        col: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        div()
            .text_xs()
            .text_color(cx.theme().foreground)
            .overflow_hidden()
            .text_ellipsis()
            .child(self.label(row, col))
    }

    /// Return the full logical value for keyboard and accessibility navigation.
    fn cell_label(&self, row: usize, col: usize, _: &App) -> Option<SharedString> {
        self.value(row, col).map(Into::into)
    }

    /// Empty detail relations are valid, particularly for empty files.
    fn render_empty(
        &mut self,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(tr!("info.no_entries"))
    }
}

/// Localized headers in logical schema column order.
fn header(column: usize) -> SharedString {
    match column {
        0 => tr!("metadata.name"),
        1 => tr!("metadata.data_type"),
        2 => tr!("metadata.nullable"),
        _ => SharedString::default(),
    }
}

/// Describe logical types using the main grid vocabulary. A question mark inside a
/// list describes the element, independently of the enclosing field's nullability.
fn detailed_type(ty: &DataType) -> String {
    let label = cells::type_label(ty);
    match ty {
        DataType::List(f)
        | DataType::LargeList(f)
        | DataType::ListView(f)
        | DataType::LargeListView(f)
        | DataType::FixedSizeList(f, _) => {
            let size = if let DataType::FixedSizeList(_, n) = ty {
                format!("; {n}")
            } else {
                String::new()
            };
            format!(
                "{label}<{}{}{size}>",
                detailed_type(f.data_type()),
                if f.is_nullable() { "?" } else { "" }
            )
        }
        DataType::Dictionary(_, value) => detailed_type(value),
        DataType::Decimal32(p, s)
        | DataType::Decimal64(p, s)
        | DataType::Decimal128(p, s)
        | DataType::Decimal256(p, s) => format!("{label}({p}, {s})"),
        DataType::FixedSizeBinary(n) => format!("{}({n})", tr!("types.binary")),
        _ if label == tr!("types.value") => ty.to_string(),
        _ => label.to_string(),
    }
}

/// Truncate at a Unicode character boundary only for previews, with a visible ellipsis.
pub(super) fn limited_text(text: &str, limit: Option<usize>) -> String {
    let Some(limit) = limit else {
        return text.to_string();
    };
    let mut chars = text.chars();
    let mut result: String = chars.by_ref().take(limit).collect();
    if chars.next().is_some() {
        result.push('…');
    }
    result
}

#[cfg(test)]
mod tests {
    use super::{DataType, detailed_type, tr};
    use arrow_schema::Field;

    /// Render nullable elements and exact decimal parameters inside list types.
    #[test]
    fn nested_type_keeps_precision_and_element_nullability() {
        for (nullable, suffix) in [(true, "?"), (false, "")] {
            let element = Field::new("element", DataType::Decimal128(18, 2), nullable);
            assert_eq!(
                detailed_type(&DataType::List(element.into())),
                format!(
                    "{}<{}(18, 2){suffix}>",
                    tr!("types.list"),
                    tr!("types.decimal")
                )
            );
        }
    }
}
