//! Parquet view registration, footer summaries and bounded metadata reads via DuckDB.

use crate::{
    Cancellation, ParquetPage, ParquetRequest, ParquetSection, ParquetSummary, Preview, QueryError,
};
use duckdb::Connection;

/// Bind the already parameterized source path to the connection-local data view.
pub(super) fn register(conn: &Connection) -> Result<(), QueryError> {
    Ok(conn.execute_batch(
        "CREATE VIEW current_file AS SELECT * FROM read_parquet(getvariable('source_path'))",
    )?)
}

/// Read footer row counts and file properties without scanning data or running COUNT.
/// Caller supplies the Arrow top-level column count; nested leaf counts are distinct.
pub(super) fn summary(
    conn: &Connection,
    columns: usize,
) -> Result<(usize, ParquetSummary), QueryError> {
    let (rows, groups, size, created_by, version): (i64, i64, i64, Option<String>, i64) = conn.query_row(
        "SELECT num_rows, num_row_groups, file_size_bytes, created_by, format_version FROM parquet_file_metadata(getvariable('source_path'))",
        [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
    )?;
    let invalid =
        || QueryError::Invalid("Parquet footer contains an unsupported count or size".into());
    Ok((
        usize::try_from(rows).map_err(|_| invalid())?,
        ParquetSummary {
            file_bytes: u64::try_from(size).map_err(|_| invalid())?,
            columns,
            row_groups: u64::try_from(groups).map_err(|_| invalid())?,
            created_by,
            format_version: version,
        },
    ))
}

/// Read one metadata page with lookahead and an 8 MiB retained Arrow-buffer ceiling.
/// Runs on a worker with an interrupt monitor. Limits bound returned data, while DuckDB
/// still parses the file footer internally. Raw key/value blobs and optional statistics
/// remain typed; oversized records fail explicitly rather than silently truncating values.
/// Schema pages omit the first (root) node before pagination, retaining original node IDs.
pub(super) fn page(
    conn: &Connection,
    request: ParquetRequest,
    cancel: &Cancellation,
) -> Result<ParquetPage, QueryError> {
    let relation = match request.section {
        ParquetSection::Schema => {
            // Skip by position, not name: actual fields may also be named root or schema.
            "SELECT row_number() OVER () AS node, name, type AS physical_type, repetition_type, num_children, logical_type, converted_type, duckdb_type, type_length, precision, scale, field_id FROM parquet_schema(getvariable('source_path')) QUALIFY node > 1"
        }
        ParquetSection::RowGroups => {
            "SELECT row_group_id, path_in_schema, column_id, row_group_num_rows, row_group_num_columns, compression, encodings, total_compressed_size, total_uncompressed_size, num_values, stats_min_value, stats_max_value, min_is_exact, max_is_exact, stats_null_count, stats_distinct_count, data_page_offset, dictionary_page_offset FROM parquet_metadata(getvariable('source_path'))"
        }
        ParquetSection::KeyValue => {
            "SELECT row_number() OVER () AS entry, key, value FROM parquet_kv_metadata(getvariable('source_path'))"
        }
    };
    let order = match request.section {
        ParquetSection::Schema => "node",
        ParquetSection::RowGroups => "row_group_id, column_id",
        ParquetSection::KeyValue => "entry",
    };
    // Only static relation fragments are interpolated; offsets and paths remain bound values.
    let sql = format!("SELECT * FROM ({relation}) ORDER BY {order} LIMIT ? OFFSET ?");
    let mut statement = conn.prepare(&sql)?;
    let stream = statement.stream_arrow([
        (ParquetRequest::PAGE_SIZE + 1) as i64,
        request.offset() as i64,
    ])?;
    let schema = stream.get_schema();
    let mut batches = Vec::new();
    let mut rows = 0;
    let mut bytes = 0usize;
    let mut has_next = false;
    for batch in stream {
        cancel.check()?;
        let batch = batch?;
        bytes = bytes.saturating_add(batch.get_array_memory_size());
        if bytes > 8 * 1024 * 1024 {
            return Err(QueryError::Invalid(
                "Parquet metadata page exceeds the 8 MiB memory budget".into(),
            ));
        }
        let retained = batch.num_rows().min(ParquetRequest::PAGE_SIZE - rows);
        has_next |= retained < batch.num_rows();
        if retained > 0 {
            batches.push(batch.slice(0, retained));
            rows += retained;
        }
    }
    cancel.check()?;
    Ok(ParquetPage {
        preview: Preview {
            schema,
            batches,
            rows,
            total_rows: None,
            truncated: false,
        },
        has_next,
    })
}
