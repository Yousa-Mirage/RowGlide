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
    /// Exact number of row groups recorded in the footer.
    pub row_groups: u64,
    /// Optional producer identification from the footer; not inferred from the filename.
    pub created_by: Option<String>,
    /// Parquet format version recorded in the footer.
    pub format_version: i64,
}

/// Detailed metadata relation requested independently of Browse and SQL results.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ParquetSection {
    /// Original flattened schema, including group nodes, repetition and logical types.
    #[default]
    Schema,
    /// One record per row-group column chunk, including optional statistics.
    RowGroups,
    /// Original file-level key/value bytes; neither key nor value is assumed to be UTF-8.
    KeyValue,
}

/// Validated fixed-size metadata page, separate from user data pagination.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParquetRequest {
    /// Which metadata relation to inspect.
    pub section: ParquetSection,
    /// Zero-based relation offset; private so callers cannot bypass validation.
    offset: usize,
}

impl ParquetRequest {
    /// Maximum displayed metadata records per page; the backend fetches one extra for navigation.
    pub const PAGE_SIZE: usize = 100;

    /// Validate an offset before converting it to the engine's signed integer range.
    pub fn new(section: ParquetSection, offset: usize) -> Result<Self, QueryError> {
        if offset > (i64::MAX as usize).saturating_sub(Self::PAGE_SIZE + 1) {
            return Err(QueryError::Invalid("Metadata offset is too large".into()));
        }
        Ok(Self { section, offset })
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
