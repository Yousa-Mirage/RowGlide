//! Real Parquet footer inspection, typed metadata paging and error boundaries.

use arrow_array::{Array, BinaryArray, Int64Array, StringArray};
use duckdb::Connection;
use rowglide_core::{
    Backend, Cancellation, DuckDbBackend, ParquetRequest, ParquetSection, QueryError,
};
use std::{error::Error, fs};

/// Inspect nested schema and exact footer totals independently of query results.
#[test]
fn summary_schema_and_column_chunks() -> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("nested ' data.parquet");
    let conn = Connection::open_in_memory()?;
    conn.execute("COPY (SELECT i AS id, [i, NULL] AS items, {'value': i} AS profile FROM range(5000) t(i)) TO ? (FORMAT PARQUET, COMPRESSION ZSTD, ROW_GROUP_SIZE 2048)", [path.to_string_lossy().as_ref()])?;
    let backend = DuckDbBackend::default();
    let source = backend.open(&path, Cancellation::default())?;
    assert_eq!(source.rows, Some(5000));
    let summary = source.parquet.ok_or("Missing summary")?;
    assert_eq!(summary.columns, 3);
    assert!(summary.row_groups >= 2);
    assert_eq!(summary.file_bytes, fs::metadata(&path)?.len());
    assert!(summary.created_by.is_some());
    let schema = backend.parquet_metadata(
        source.id,
        ParquetRequest::new(ParquetSection::Schema, 0)?,
        Cancellation::default(),
    )?;
    assert!(schema.preview.rows > 3);
    assert!(!schema.has_next);
    assert!(
        schema
            .preview
            .schema
            .field_with_name("repetition_type")
            .is_ok()
    );
    let chunks = backend.parquet_metadata(
        source.id,
        ParquetRequest::new(ParquetSection::RowGroups, 0)?,
        Cancellation::default(),
    )?;
    assert_eq!(chunks.preview.rows as u64, summary.row_groups * 3);
    assert!(
        chunks
            .preview
            .schema
            .field_with_name("min_is_exact")
            .is_ok()
    );
    for batch in chunks.preview.batches {
        let index = batch.schema().index_of("compression")?;
        let compression = batch
            .column(index)
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or("Expected compression strings")?;
        assert!((0..compression.len()).all(|i| compression.value(i) == "ZSTD"));
    }
    Ok(())
}

/// Page wide file schemas without dropping or duplicating nodes; empty files retain columns.
#[test]
fn wide_schema_paging_and_empty_file() -> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("wide.parquet");
    let columns = (0..205)
        .map(|i| format!("{i}::BIGINT AS c{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    let conn = Connection::open_in_memory()?;
    conn.execute(
        &format!("COPY (SELECT {columns} WHERE false) TO ? (FORMAT PARQUET)"),
        [path.to_string_lossy().as_ref()],
    )?;
    let backend = DuckDbBackend::default();
    let source = backend.open(&path, Cancellation::default())?;
    assert_eq!(source.rows, Some(0));
    assert_eq!(source.parquet.ok_or("Missing summary")?.columns, 205);
    let mut nodes = Vec::new();
    for offset in [0, 100, 200] {
        let page = backend.parquet_metadata(
            source.id,
            ParquetRequest::new(ParquetSection::Schema, offset)?,
            Cancellation::default(),
        )?;
        assert_eq!(page.has_next, offset < 200);
        assert!(page.preview.rows <= ParquetRequest::PAGE_SIZE);
        for batch in page.preview.batches {
            let ids = batch
                .column(0)
                .as_any()
                .downcast_ref::<Int64Array>()
                .ok_or("Missing node IDs")?;
            nodes.extend(ids.values().iter().copied());
        }
    }
    assert_eq!(nodes, (1..=206).collect::<Vec<_>>());
    let page = backend.parquet_metadata(
        source.id,
        ParquetRequest::new(ParquetSection::KeyValue, 0)?,
        Cancellation::default(),
    )?;
    assert_eq!(page.preview.rows, 0);
    assert_eq!(page.preview.schema.fields().len(), 3);
    Ok(())
}

/// Preserve original key/value blobs and surface cancellation and unsupported-source errors.
#[test]
fn metadata_bytes_cancellation_and_errors() -> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("metadata.parquet");
    let conn = Connection::open_in_memory()?;
    conn.execute(
        "COPY (SELECT 1 AS id) TO ? (FORMAT PARQUET, KV_METADATA {author: 'Rowglide', empty: ''})",
        [path.to_string_lossy().as_ref()],
    )?;
    let backend = DuckDbBackend::default();
    let source = backend.open(&path, Cancellation::default())?;
    let request = ParquetRequest::new(ParquetSection::KeyValue, 0)?;
    let page = backend.parquet_metadata(source.id, request, Cancellation::default())?;
    assert_eq!(page.preview.rows, 2);
    let mut pairs = Vec::new();
    for batch in page.preview.batches {
        let keys = batch
            .column(1)
            .as_any()
            .downcast_ref::<BinaryArray>()
            .ok_or("Keys must remain binary")?;
        let values = batch
            .column(2)
            .as_any()
            .downcast_ref::<BinaryArray>()
            .ok_or("Values must remain binary")?;
        for row in 0..batch.num_rows() {
            pairs.push((keys.value(row).to_vec(), values.value(row).to_vec()));
        }
    }
    assert!(pairs.contains(&(b"author".to_vec(), b"Rowglide".to_vec())));
    assert!(pairs.contains(&(b"empty".to_vec(), Vec::new())));
    let cancel = Cancellation::default();
    cancel.cancel();
    assert!(matches!(
        backend.parquet_metadata(source.id, request, cancel),
        Err(QueryError::Cancelled)
    ));
    assert!(
        backend
            .parquet_metadata(999, request, Cancellation::default())
            .is_err()
    );
    let csv = temp.path().join("plain.csv");
    fs::write(&csv, "id\n1\n")?;
    let csv = backend.open(&csv, Cancellation::default())?;
    assert!(
        backend
            .parquet_metadata(csv.id, request, Cancellation::default())
            .is_err()
    );
    fs::remove_file(path)?;
    assert!(
        backend
            .parquet_metadata(source.id, request, Cancellation::default())
            .is_err()
    );
    assert!(ParquetRequest::new(ParquetSection::Schema, usize::MAX).is_err());
    Ok(())
}

/// A single huge metadata value fails the page budget without damaging later requests.
#[test]
fn oversized_metadata_is_explicit_and_recoverable() -> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("large-metadata.parquet");
    let conn = Connection::open_in_memory()?;
    let value = "x".repeat(9 * 1024 * 1024);
    conn.execute(
        "COPY (SELECT 1 AS id) TO ? (FORMAT PARQUET, KV_METADATA {large: ?})",
        duckdb::params![path.to_string_lossy().as_ref(), value],
    )?;
    let backend = DuckDbBackend::default();
    let source = backend.open(&path, Cancellation::default())?;
    let error = backend
        .parquet_metadata(
            source.id,
            ParquetRequest::new(ParquetSection::KeyValue, 0)?,
            Cancellation::default(),
        )
        .err()
        .ok_or("Oversized metadata succeeded")?;
    assert!(error.to_string().contains("8 MiB"));
    let schema = backend.parquet_metadata(
        source.id,
        ParquetRequest::new(ParquetSection::Schema, 0)?,
        Cancellation::default(),
    )?;
    assert_eq!(schema.preview.rows, 2);
    Ok(())
}
