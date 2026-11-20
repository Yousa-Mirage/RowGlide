//! Engine-independent Parquet file summaries and bounded detail-page requests.

use crate::{Preview, QueryError};

/// File-level footer information captured when a Parquet source is opened.
/// Column count describes top-level Arrow fields, not flattened Parquet leaf columns.
#[derive(Clone, Debug)]
pub struct ParquetSummary {
    /// Physical source file size in bytes, independent of result cache storage.
    pub file_bytes: u64,
    /// Number of top-level fields exposed by the file reader.
    pub columns: usize,
    /// Optional producer identification from the footer; not inferred from the filename.
    pub created_by: Option<String>,
    /// Parquet format version recorded in the footer.
    pub format_version: i64,
}

/// A validated schema detail request, separate from user data pagination.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParquetRequest {
    /// Zero-based relation offset; private so callers cannot bypass validation.
    offset: usize,
}

impl ParquetRequest {
    /// Maximum displayed metadata records per page; the backend fetches one extra for navigation.
    pub const PAGE_SIZE: usize = 100;

    /// Validate an offset before converting it to the engine's signed integer range.
    pub fn new(offset: usize) -> Result<Self, QueryError> {
        if offset > (i64::MAX as usize).saturating_sub(Self::PAGE_SIZE + 1) {
            return Err(QueryError::Invalid("Metadata offset is too large".into()));
        }
        Ok(Self { offset })
    }

    /// Return the zero-based offset of the requested metadata records.
    pub fn offset(self) -> usize {
        self.offset
    }
}

/// One bounded, typed metadata page; missing statistics remain Arrow nulls.
pub struct ParquetPage {
    /// Up to PAGE_SIZE records and their metadata schema, including empty relations.
    pub preview: Preview,
    /// True only when lookahead observed another record after this page.
    pub has_next: bool,
}
