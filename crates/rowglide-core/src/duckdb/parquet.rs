//! Parquet view registration, footer summaries and bounded metadata reads via DuckDB.

use crate::{Cancellation, ParquetPage, ParquetRequest, ParquetSummary, QueryError, SchemaRow};
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
    let (rows, size, created_by, version): (i64, i64, Option<String>, i64) = conn.query_row(
        "SELECT num_rows, file_size_bytes, created_by, format_version FROM parquet_file_metadata(getvariable('source_path'))",
        [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    let invalid =
        || QueryError::Invalid("Parquet footer contains an unsupported count or size".into());
    Ok((
        usize::try_from(rows).map_err(|_| invalid())?,
        ParquetSummary {
            file_bytes: u64::try_from(size).map_err(|_| invalid())?,
            columns,
            created_by,
            format_version: version,
        },
    ))
}

/// Decode the footer's logical Arrow schema without reading data pages. DuckDB's query
/// result schema can mark required fields nullable, so it cannot supply this detail.
pub(super) fn page(
    path: &std::path::Path,
    request: ParquetRequest,
    cancel: &Cancellation,
) -> Result<ParquetPage, QueryError> {
    use parquet::arrow::arrow_reader::ArrowReaderMetadata;
    let file = std::fs::File::open(path)?;
    let metadata = ArrowReaderMetadata::load(&file, Default::default())?;
    let mut rows = Vec::new();
    let mut skip = request.offset();
    let mut bytes = 0_usize;
    let mut levels = vec![metadata.schema().fields().iter()];
    while let Some(level) = levels.last_mut() {
        cancel.check()?;
        let Some(field) = level.next() else {
            levels.pop();
            continue;
        };
        if skip > 0 {
            skip -= 1;
        } else {
            if rows.len() == ParquetRequest::PAGE_SIZE {
                return Ok(ParquetPage {
                    rows,
                    has_next: true,
                });
            }
            bytes = bytes.saturating_add(field.size());
            if bytes > 8 * 1024 * 1024 {
                return Err(QueryError::Invalid(
                    "Parquet schema page exceeds the 8 MiB memory budget".into(),
                ));
            }
            rows.push(SchemaRow {
                field: field.clone(),
                depth: levels.len() - 1,
            });
        }
        if let Some(children) = named_children(field.data_type()) {
            levels.push(children.iter());
        }
    }
    Ok(ParquetPage {
        rows,
        has_next: false,
    })
}

/// Descend through element and map-entry wrappers until genuine named children are found.
fn named_children(mut ty: &arrow_schema::DataType) -> Option<&arrow_schema::Fields> {
    use arrow_schema::DataType::*;
    loop {
        ty = match ty {
            Struct(fields) => return Some(fields),
            List(f)
            | LargeList(f)
            | ListView(f)
            | LargeListView(f)
            | FixedSizeList(f, _)
            | Map(f, _) => f.data_type(),
            Dictionary(_, value) => value,
            _ => return None,
        };
    }
}
