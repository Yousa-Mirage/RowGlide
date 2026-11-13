//! Engine-independent contracts. Blocking backend operations run on a worker.

mod pagination;

pub use pagination::{BrowsePage, PageResult};

use arrow_array::RecordBatch;
use arrow_schema::SchemaRef;
use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[derive(Clone, Debug)]
pub struct Source {
    pub id: usize,
    pub name: String,
    pub format: &'static str,
    pub rows: Option<usize>,
    pub description: &'static str,
}

/// Sorting request
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sort {
    pub column: usize,
    pub descending: bool,
}

/// Browse parameters or an explicit SQL request for a dataset.
#[derive(Clone, Debug, Default)]
pub struct Query {
    pub source: usize,
    pub search: String,
    pub sort: Option<Sort>,

    /// Browse row limit, applied after filtering and sorting; None is unlimited.
    /// SQL requests use their own query semantics instead.
    pub limit: Option<usize>,

    /// Explicit browse offset, not an automatic pagination strategy.
    pub offset: usize,

    /// SQL is executed in the current_file context; mock implementations accept presets only.
    pub sql: Option<String>,
}

/// Shared cancellation flag checked cooperatively by background work.
#[derive(Clone, Default)]
pub struct Cancellation(Arc<AtomicBool>);

impl Cancellation {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn check(&self) -> Result<(), QueryError> {
        if self.0.load(Ordering::Relaxed) {
            Err(QueryError::Cancelled)
        } else {
            Ok(())
        }
    }
}

#[derive(Debug)]
pub enum QueryError {
    Cancelled,
    Invalid(String),
    Data(arrow_schema::ArrowError),
    Backend(String),
}

impl std::fmt::Display for QueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => write!(f, "Query cancelled"),
            Self::Invalid(message) => f.write_str(message),
            Self::Backend(message) => f.write_str(message),
            Self::Data(error) => write!(f, "Cannot build data batch: {error}"),
        }
    }
}

impl std::error::Error for QueryError {}

impl From<arrow_schema::ArrowError> for QueryError {
    fn from(value: arrow_schema::ArrowError) -> Self {
        Self::Data(value)
    }
}

/// Pull-based delivery provides backpressure. Dropping the cursor releases it.
pub trait BatchCursor: Send {
    fn schema(&self) -> SchemaRef;
    fn total_rows(&self) -> Option<usize>;
    fn next_batch(&mut self) -> Result<Option<RecordBatch>, QueryError>;
}

/// Blocking data operations; callers must execute queries off the UI thread.
pub trait Backend: Send + Sync {
    fn sources(&self) -> Vec<Source>;
    /// Register and inspect a local file on a background worker.
    fn open(&self, _path: &Path, _cancel: Cancellation) -> Result<Source, QueryError> {
        Err(QueryError::Invalid(
            "This backend cannot open local files".into(),
        ))
    }
    fn query(
        &self,
        query: Query,
        cancellation: Cancellation,
    ) -> Result<Box<dyn BatchCursor>, QueryError>;
}

/// Bounded first-page collection for the UI prototype, not a full result cache.
pub struct Preview {
    pub schema: SchemaRef,
    pub batches: Vec<RecordBatch>,
    pub rows: usize,
    pub total_rows: Option<usize>,
    pub truncated: bool,
}

impl Preview {
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

    /// Locate a visible row within the retained Arrow batches.
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
