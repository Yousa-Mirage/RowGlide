//! Virtualized footer-detail table and lossless text/hex formatting for selected values.

use arrow_array::{
    Array, ArrayRef, BinaryArray, LargeBinaryArray, LargeStringArray, StringArray, StringViewArray,
};
use gpui_kit::component::{
    ActiveTheme,
    table::{Column, TableDelegate, TableState},
};
use gpui_kit::*;
use rowglide_core::{ParquetPage, QueryError};

/// Retains only the visible metadata page; original Arrow buffers preserve nulls and blobs.
#[derive(Default)]
pub(super) struct MetadataGrid {
    /// Current page, cleared before loading another section, source or offset.
    pub(super) page: Option<ParquetPage>,
    /// Loading is separate from the main data table's query lifecycle.
    pub(super) loading: bool,
}

impl MetadataGrid {
    /// Measure bounded page previews and the header using the UI font.
    /// Widths are computed on table refresh, not on each rendered cell. Multiline
    /// values use their widest line; the column separately caps excessive widths.
    fn content_width(&self, column: usize, cx: &App) -> Pixels {
        let header = self
            .page
            .as_ref()
            .and_then(|page| page.preview.schema.fields().get(column))
            .map_or_else(String::new, |field| field.name().replace('_', " "));
        let labels = std::iter::once(header)
            .chain((0..self.rows_count(cx)).map(|row| self.label(row, column)));
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

    /// Borrow one typed cell by page-local coordinates; stale/out-of-range selections are absent.
    pub(super) fn cell(&self, row: usize, column: usize) -> Option<(&ArrayRef, usize)> {
        let (batch, local) = self.page.as_ref()?.preview.row(row)?;
        Some((batch.columns().get(column)?, local))
    }

    /// Format one visible cell with bounded text; raw retained buffers remain unchanged.
    fn label(&self, row: usize, column: usize) -> String {
        self.cell(row, column)
            .map_or_else(String::new, |(array, local)| {
                value_text(array, local, Some(160)).unwrap_or_else(|error| error.to_string())
            })
    }
}

impl TableDelegate for MetadataGrid {
    /// Expose the page schema even when the metadata relation contains no records.
    fn columns_count(&self, _: &App) -> usize {
        self.page
            .as_ref()
            .map_or(0, |page| page.preview.schema.fields().len())
    }

    /// Expose only retained metadata records, never source-data rows.
    fn rows_count(&self, _: &App) -> usize {
        self.page.as_ref().map_or(0, |page| page.preview.rows)
    }

    /// Fit headers and current-page previews within readable width bounds.
    /// Wide relations remain horizontally scrollable; backend order defines pagination.
    fn column(&self, index: usize, cx: &App) -> Column {
        let name = self
            .page
            .as_ref()
            .and_then(|page| page.preview.schema.fields().get(index))
            .map_or("Unavailable", |field| field.name().as_str());
        Column::new(format!("metadata-{index}"), name.replace('_', " "))
            .width((self.content_width(index, cx) + px(24.)).clamp(px(64.), px(320.)))
            .min_width(px(64.))
            .max_width(px(640.))
            .movable(false)
    }

    /// Refit a manually resized column on double-click, including the retained page only.
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
        let is_null = self
            .cell(row, col)
            .is_some_and(|(array, local)| array.is_null(local));
        let color = if is_null {
            cx.theme().muted_foreground
        } else {
            cx.theme().foreground
        };
        div()
            .text_xs()
            .text_color(color)
            .overflow_hidden()
            .text_ellipsis()
            .child(self.label(row, col))
    }

    /// Return the same bounded label for keyboard and accessibility navigation.
    fn cell_label(&self, row: usize, col: usize, _: &App) -> Option<SharedString> {
        self.cell(row, col)?;
        Some(self.label(row, col).into())
    }

    /// Empty metadata is valid, particularly for custom key/value pairs and zero row groups.
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
            .child("No metadata entries")
    }
}

/// Format a metadata value for preview or copying without changing its stored Arrow type.
/// UTF-8 blobs display as text; other blobs use reversible 0x-prefixed hexadecimal.
/// `limit` bounds preview characters and explicitly marks truncation; None copies the full value.
pub(super) fn value_text(
    array: &ArrayRef,
    row: usize,
    limit: Option<usize>,
) -> Result<String, QueryError> {
    if row >= array.len() {
        return Err(QueryError::Invalid(
            "Metadata cell is no longer available".into(),
        ));
    }
    if array.is_null(row) {
        return Ok("null".into());
    }
    let binary = array
        .as_any()
        .downcast_ref::<BinaryArray>()
        .map(|array| array.value(row))
        .or_else(|| {
            array
                .as_any()
                .downcast_ref::<LargeBinaryArray>()
                .map(|array| array.value(row))
        });
    if let Some(bytes) = binary {
        return Ok(match std::str::from_utf8(bytes) {
            Ok(text) => limited_text(text, limit),
            Err(_) => {
                let count = limit
                    .map_or(bytes.len(), |limit| limit.saturating_sub(2) / 2)
                    .min(bytes.len());
                const HEX: &[u8; 16] = b"0123456789abcdef";
                let mut text = String::with_capacity(2 + count * 2 + 3);
                text.push_str("0x");
                for byte in &bytes[..count] {
                    text.push(HEX[(byte >> 4) as usize] as char);
                    text.push(HEX[(byte & 0x0f) as usize] as char);
                }
                if count < bytes.len() {
                    text.push('…');
                }
                text
            }
        });
    }
    let string = array
        .as_any()
        .downcast_ref::<StringArray>()
        .map(|array| array.value(row))
        .or_else(|| {
            array
                .as_any()
                .downcast_ref::<LargeStringArray>()
                .map(|array| array.value(row))
        })
        .or_else(|| {
            array
                .as_any()
                .downcast_ref::<StringViewArray>()
                .map(|array| array.value(row))
        });
    if let Some(text) = string {
        return Ok(limited_text(text, limit));
    }
    Ok(limited_text(
        &arrow_cast::display::array_value_to_string(array.as_ref(), row)?,
        limit,
    ))
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
    use super::value_text;
    use arrow_array::{ArrayRef, BinaryArray};
    use rowglide_core::QueryError;
    use std::sync::Arc;

    /// Previews are bounded, while copying preserves full Unicode text, arbitrary bytes and nulls.
    #[test]
    fn binary_values_and_full_copy() -> Result<(), QueryError> {
        let array: ArrayRef = Arc::new(BinaryArray::from(vec![
            Some("中文内容".as_bytes()),
            Some(&[0xff, 0x00, 0x01][..]),
            Some(&[][..]),
            None,
        ]));
        assert_eq!(value_text(&array, 0, Some(2))?, "中文…");
        assert_eq!(value_text(&array, 0, None)?, "中文内容");
        assert_eq!(value_text(&array, 1, Some(4))?, "0xff…");
        assert_eq!(value_text(&array, 1, None)?, "0xff0001");
        assert_eq!(value_text(&array, 2, None)?, "");
        assert_eq!(value_text(&array, 3, None)?, "null");
        assert!(value_text(&array, 4, None).is_err());
        Ok(())
    }
}
