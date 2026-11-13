//! Source metadata, query descriptions and retained Arrow results.

use crate::{BatchCursor, Cancellation, QueryError};
use arrow_array::RecordBatch;
use arrow_schema::SchemaRef;

/// Metadata for a dataset registered with one backend instance.
///
/// The identifier is backend-local, not a persistent file ID. This record contains
/// no Arrow buffers or open file handles; an unknown row count stays `None`.
#[derive(Clone, Debug)]
pub struct Source {
    /// Backend-local handle used by `Query::source`.
    pub id: usize,
    /// Human-readable file or fixture name displayed in Folder Tabs.
    pub name: String,
    /// Reader format label, currently `PARQUET` or `CSV` for local files.
    pub format: &'static str,
    /// Exact source row count when known without a count scan; otherwise unknown.
    pub rows: Option<usize>,
    /// Short source description, independent of query status or filtering.
    pub description: &'static str,
}

/// A single-column ordering applied to the complete filtered Browse result.
///
/// The column index refers to the source schema. Backends keep nulls last in both
/// directions and use source order to resolve ties before selecting a page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sort {
    /// Zero-based index into the registered source schema.
    pub column: usize,
    /// Descending when true, ascending when false; nulls remain last.
    pub descending: bool,
}

/// Engine-independent request for one registered source.
///
/// Browse uses text filtering, column sorting and an optional row range. SQL mode
/// supplies an explicit statement against `current_file`; the UI clears Browse
/// fields for that mode. `Default` selects source zero with no Browse row limit.
#[derive(Clone, Debug, Default)]
pub struct Query {
    /// Registered source ID within the backend handling this request.
    pub source: usize,
    /// Case-insensitive substring searched across supported text columns in Browse.
    pub search: String,
    /// Optional global Browse sort; `None` retains source order.
    pub sort: Option<Sort>,

    /// Browse row limit, applied after filtering and sorting; None is unlimited.
    /// SQL requests use their own query semantics instead.
    pub limit: Option<usize>,

    /// Rows skipped after Browse filtering/sorting, before retaining the page.
    /// The UI changes this through explicit page navigation or Apply.
    pub offset: usize,

    /// SQL is executed in the current_file context; mock implementations accept presets only.
    pub sql: Option<String>,
}

/// Typed Arrow buffers retained for display, independent of source metadata.
///
/// This container represents either a complete Browse page from `BrowsePage::load`
/// or an explicit bounded preview from `Self::collect`. Its row count describes
/// only retained rows; it is not the total number of matching source rows.
pub struct Preview {
    /// Query-result schema, available even when no rows were retained.
    pub schema: SchemaRef,
    /// Typed result batches in display order; their buffers remain shared by Arrow.
    pub batches: Vec<RecordBatch>,
    /// Number of retained rows across `batches`.
    pub rows: usize,
    /// Optional exact cursor-result total; Browse page collection leaves it unknown.
    pub total_rows: Option<usize>,
    /// Whether legacy collection stopped at a budget boundary after fetching a batch.
    /// Complete Browse pages leave this false and expose `PageResult::has_next` instead.
    pub truncated: bool,
}

impl Preview {
    /// Collect a legacy preview of at most 8,192 rows and 16 MiB of batch buffers.
    ///
    /// This blocking helper remains for explicit previews and tests. Production SQL uses
    /// `crate::QueryResult`; Browse pagination uses `crate::BrowsePage`.
    /// It sets `truncated` when collection stops at a budget boundary, preserves
    /// the cursor's optional total, and drops the cursor on every return path.
    ///
    /// # Errors
    ///
    /// Propagates cancellation and batch-reading errors. Budget exhaustion produces a
    /// successful, explicitly truncated preview rather than a complete result.
    pub fn collect(
        mut cursor: Box<dyn BatchCursor>,
        cancellation: &Cancellation,
    ) -> Result<Self, QueryError> {
        const MAX_ROWS: usize = 8192;
        const MAX_BYTES: usize = 16 * 1024 * 1024;

        let mut preview = Self {
            schema: cursor.schema(),
            total_rows: cursor.total_rows(),
            batches: vec![],
            rows: 0,
            truncated: false,
        };
        let mut bytes = 0;

        loop {
            cancellation.check()?;
            let Some(batch) = cursor.next_batch()? else {
                break;
            };

            // Bound retained rows and memory even when the source is larger.
            let remaining = MAX_ROWS - preview.rows;
            if remaining == 0 || bytes + batch.get_array_memory_size() > MAX_BYTES {
                preview.truncated = true;
                break;
            }

            let keep = remaining.min(batch.num_rows());
            bytes += batch.get_array_memory_size();
            preview.rows += keep;
            preview.batches.push(batch.slice(0, keep));
            if keep < batch.num_rows() {
                preview.truncated = true;
                break;
            }
        }

        Ok(preview)
    }

    /// Locate a zero-based retained-result row and its offset inside an Arrow batch.
    ///
    /// Returns `None` for an out-of-range index. The lookup walks batch lengths without
    /// copying data; the returned reference remains borrowed from this preview.
    pub fn row(&self, mut row: usize) -> Option<(&RecordBatch, usize)> {
        for batch in &self.batches {
            if row < batch.num_rows() {
                return Some((batch, row));
            }
            row -= batch.num_rows();
        }

        None
    }
}
