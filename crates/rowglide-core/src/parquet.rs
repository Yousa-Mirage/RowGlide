//! Engine-independent Parquet file summaries and logical schema fields.

use arrow_schema::FieldRef;

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

/// One logical field in file order; wrapper names used by Parquet encodings are absent.
pub struct SchemaRow {
    /// Original field, including its declared nullability and nested element types.
    pub field: FieldRef,
    /// Indentation level; top-level columns start at zero.
    pub depth: usize,
}
