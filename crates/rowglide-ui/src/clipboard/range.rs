//! Immutable rectangular copy targets; rows are read one Arrow batch at a time.

use crate::{i18n::tr_string, table::Grid};
use arrow_array::RecordBatch;
use gpui_kit::App;
use gpui_kit::component::table::TableDelegate;
use rowglide_core::{Cancellation, CopyError, CopyFormat, CopyLimits, QueryResult, serialize_cell};
use std::{collections::HashSet, ops::RangeInclusive, sync::Arc};

/// Two display coordinates defining an inclusive rectangle in either drag direction.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct CellRange {
    /// Fixed origin used by dragging and repeated Shift clicks.
    pub(crate) anchor: (usize, usize),
    /// Current pointer destination.
    pub(crate) end: (usize, usize),
}

impl CellRange {
    /// Row bounds in top-to-bottom result order.
    fn rows(self) -> RangeInclusive<usize> {
        self.anchor.0.min(self.end.0)..=self.anchor.0.max(self.end.0)
    }

    /// Column bounds in left-to-right visible order.
    fn columns(self) -> RangeInclusive<usize> {
        self.anchor.1.min(self.end.1)..=self.anchor.1.max(self.end.1)
    }

    /// Whether a display cell belongs to the current multi-cell selection.
    pub(crate) fn includes(self, row: usize, column: usize) -> bool {
        self.anchor != self.end && self.rows().contains(&row) && self.columns().contains(&column)
    }
}

/// A captured selection in a single result; its data survives later tab and page changes.
pub(crate) struct RangeCopy {
    /// Owned handles to the original result.
    data: RangeData,
    /// Original schema positions and names in display order.
    fields: Vec<(usize, String)>,
    /// Inclusive result-relative row bounds.
    rows: RangeInclusive<usize>,
}

/// Browse retains selected batch slices, while SQL retains its spill-aware result handle.
enum RangeData {
    /// Shared Arrow slices covering exactly the requested rows.
    Browse(Vec<RecordBatch>),
    /// SQL rows may need an asynchronous disk read.
    Sql(Arc<QueryResult>),
}

impl RangeCopy {
    /// Keep a streamed result alive until the selected SQL rows have been copied.
    pub(super) fn sql(
        result: Arc<QueryResult>,
        fields: Vec<(usize, String)>,
        rows: RangeInclusive<usize>,
    ) -> Self {
        Self {
            data: RangeData::Sql(result),
            fields,
            rows,
        }
    }

    /// Capture field identities and buffers only when a copy action or menu is requested.
    pub(crate) fn capture(grid: &Grid, cx: &App) -> Option<Arc<Self>> {
        let range = grid.cell_range?;
        let rows = range.rows();
        if range.anchor == range.end || *rows.end() >= grid.rows_count(cx) {
            return None;
        }
        let schema = grid.schema()?;
        let fields = range
            .columns()
            .map(|display| {
                let index = grid.schema_index(display)?;
                Some((index, schema.fields().get(index)?.name().clone()))
            })
            .collect::<Option<Vec<_>>>()?;
        if let Some(sql) = &grid.sql_result {
            return Some(Arc::new(Self::sql(sql.clone(), fields, rows)));
        }
        let data = {
            let mut start = 0;
            let mut slices = Vec::new();
            for batch in &grid.preview.as_ref()?.batches {
                let end = start + batch.num_rows();
                let lo = start.max(*rows.start());
                let hi = end.min(rows.end().saturating_add(1));
                if hi > lo {
                    slices.push(batch.slice(lo - start, hi - lo));
                }
                start = end;
                if start > *rows.end() {
                    break;
                }
            }
            RangeData::Browse(slices)
        };
        Some(Arc::new(Self { data, fields, rows }))
    }

    /// Serialize TSV with headers, or one typed JSON array per column, on a background executor.
    pub(crate) async fn serialize(
        &self,
        format: CopyFormat,
        cancel: &Cancellation,
    ) -> Result<String, CopyError> {
        let mut output = RangeOutput::new(&self.fields, format)?;
        match &self.data {
            RangeData::Browse(batches) => {
                for batch in batches {
                    output.rows(batch, 0, batch.num_rows(), &self.fields, cancel)?;
                }
            }
            RangeData::Sql(result) => {
                let mut row = *self.rows.start();
                while row <= *self.rows.end() {
                    let (batch, local) = result.row_async(row, cancel).await?;
                    let count = (batch.num_rows() - local).min(self.rows.end() - row + 1);
                    output.rows(&batch, local, count, &self.fields, cancel)?;
                    row += count;
                }
            }
        }
        output.finish()
    }
}

/// One TSV buffer or one JSON fragment per field, sharing a total clipboard byte budget.
struct RangeOutput {
    /// Wire format for values and separators.
    format: CopyFormat,
    /// Final TSV text or per-column JSON fragments read in a single pass over the data.
    parts: Vec<String>,
    /// Bytes still available, including separators and escapes.
    remaining: usize,
    /// Whether JSON arrays already contain a first value.
    started: bool,
}

impl RangeOutput {
    /// Write the header row or escaped JSON keys; reject ambiguous duplicate JSON keys.
    fn new(fields: &[(usize, String)], format: CopyFormat) -> Result<Self, CopyError> {
        let json = matches!(format, CopyFormat::Json);
        let mut output = Self {
            format,
            parts: Vec::new(),
            remaining: CopyLimits::default().max_bytes,
            started: false,
        };
        let mut names = HashSet::new();
        output.parts.push(String::new());
        for (i, (_, name)) in fields.iter().enumerate() {
            if json {
                if !names.insert(name) {
                    return Err(CopyError::Invalid(tr_string!("copy.duplicate_columns")));
                }
                if i != 0 {
                    output.parts.push(String::new());
                }
                output.push(i, if i == 0 { "{" } else { "]," })?;
                output.push(i, &serde_json::to_string(name)?)?;
                output.push(i, ":[")?;
            } else {
                if i != 0 {
                    output.push(0, "\t")?;
                }
                output.tsv(name)?;
            }
        }
        Ok(output)
    }

    /// Append a fragment only if the complete selection still fits the clipboard budget.
    fn push(&mut self, part: usize, text: &str) -> Result<(), CopyError> {
        self.remaining = self
            .remaining
            .checked_sub(text.len())
            .ok_or_else(|| CopyError::Invalid(tr_string!("copy.range_limit")))?;
        self.parts[part].push_str(text);
        Ok(())
    }

    /// Quote TSV fields containing delimiters, line breaks or quotes using doubled quotes.
    fn tsv(&mut self, text: &str) -> Result<(), CopyError> {
        if !text.contains(['\t', '\r', '\n', '"']) {
            return self.push(0, text);
        }
        self.push(0, "\"")?;
        for (index, piece) in text.split('"').enumerate() {
            if index != 0 {
                self.push(0, "\"\"")?;
            }
            self.push(0, piece)?;
        }
        self.push(0, "\"")
    }

    /// Append complete typed values without parsing numbers back through floating point.
    fn rows(
        &mut self,
        batch: &RecordBatch,
        start: usize,
        count: usize,
        fields: &[(usize, String)],
        cancel: &Cancellation,
    ) -> Result<(), CopyError> {
        let json = matches!(self.format, CopyFormat::Json);
        for row in start..start + count {
            if !json {
                self.push(0, "\n")?;
            }
            for (i, (column, _)) in fields.iter().enumerate() {
                let text = serialize_cell(
                    batch.column(*column).as_ref(),
                    row,
                    self.format,
                    CopyLimits {
                        max_bytes: self.remaining,
                        ..Default::default()
                    },
                    cancel,
                )?;
                if json {
                    if self.started {
                        self.push(i, ",")?;
                    }
                    self.push(i, &text)?;
                } else {
                    if i != 0 {
                        self.push(0, "\t")?;
                    }
                    self.tsv(&text)?;
                }
            }
            self.started = true;
        }
        Ok(())
    }

    /// Finish the last JSON array and return only a complete payload.
    fn finish(mut self) -> Result<String, CopyError> {
        if matches!(self.format, CopyFormat::Json) {
            self.push(self.parts.len() - 1, "]}")?;
            Ok(self.parts.concat())
        } else {
            Ok(self.parts.swap_remove(0))
        }
    }
}
