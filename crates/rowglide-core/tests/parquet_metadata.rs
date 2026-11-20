//! Real Parquet footer inspection, typed metadata paging and error boundaries.

use arrow_array::{Array, Int64Array, StringArray};
use duckdb::Connection;
use rowglide_core::{Backend, Cancellation, DuckDbBackend, ParquetRequest, QueryError};
use std::{error::Error, fs};

/// Inspect nested schema and exact footer totals independently of query results.
#[test]
fn summary_schema_and_column_chunks() -> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("nested ' data.parquet");
    let conn = Connection::open_in_memory()?;
    conn.execute("COPY (SELECT i AS id, [i, NULL] AS items, {'value': i} AS root FROM range(5000) t(i)) TO ? (FORMAT PARQUET, COMPRESSION ZSTD, ROW_GROUP_SIZE 2048)", [path.to_string_lossy().as_ref()])?;
    let backend = DuckDbBackend::default();
    let source = backend.open(&path, Cancellation::default())?;
    assert_eq!(source.rows, Some(5000));
    let summary = source.parquet.ok_or("Missing summary")?;
    assert_eq!(summary.columns, 3);
    assert_eq!(summary.file_bytes, fs::metadata(&path)?.len());
    assert!(summary.created_by.is_some());
    let schema =
        backend.parquet_metadata(source.id, ParquetRequest::new(0)?, Cancellation::default())?;
    let mut names = Vec::new();
    for batch in &schema.preview.batches {
        let column = batch.schema().index_of("name")?;
        let values = batch
            .column(column)
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or("Expected schema names")?;
        names.extend(values.iter().flatten().map(str::to_owned));
    }
    // Only the enclosing root is omitted; a field named root and nested groups survive.
    assert_eq!(names, ["id", "items", "list", "element", "root", "value"]);
    assert!(!schema.has_next);
    assert!(
        schema
            .preview
            .schema
            .field_with_name("repetition_type")
            .is_ok()
    );
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
            ParquetRequest::new(offset)?,
            Cancellation::default(),
        )?;
        assert_eq!(page.has_next, offset < 200);
        assert_eq!(page.preview.rows, if offset < 200 { 100 } else { 5 });
        for batch in page.preview.batches {
            let ids = batch
                .column(0)
                .as_any()
                .downcast_ref::<Int64Array>()
                .ok_or("Missing node IDs")?;
            nodes.extend(ids.values().iter().copied());
        }
    }
    assert_eq!(nodes, (2..=206).collect::<Vec<_>>());
    Ok(())
}

/// Surface cancellation and unsupported-source errors for supported footer details.
#[test]
fn detail_cancellation_and_errors() -> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("metadata.parquet");
    let conn = Connection::open_in_memory()?;
    conn.execute(
        "COPY (SELECT 1 AS id) TO ? (FORMAT PARQUET)",
        [path.to_string_lossy().as_ref()],
    )?;
    let backend = DuckDbBackend::default();
    let source = backend.open(&path, Cancellation::default())?;
    let request = ParquetRequest::new(0)?;
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
    assert!(ParquetRequest::new(usize::MAX).is_err());
    Ok(())
}

/// Closing a panel can cancel inspection while another query owns the engine gate.
#[test]
fn waiting_metadata_cancels_without_cancelling_data_query() -> Result<(), Box<dyn Error>> {
    use std::{
        sync::{Arc, mpsc},
        thread,
        time::Duration,
    };
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("busy.parquet");
    let conn = Connection::open_in_memory()?;
    conn.execute(
        "COPY (SELECT 1 AS id) TO ? (FORMAT PARQUET)",
        [path.to_string_lossy().as_ref()],
    )?;
    let backend = Arc::new(DuckDbBackend::default());
    let source = backend.open(&path, Cancellation::default())?;
    let data_cancel = Cancellation::default();
    let cursor = backend.query(
        rowglide_core::Query {
            source: source.id,
            sql: Some("SELECT i FROM range(1000000) t(i)".into()),
            ..Default::default()
        },
        data_cancel.clone(),
    )?;
    let cancel = Cancellation::default();
    let worker_cancel = cancel.clone();
    let worker_backend = backend.clone();
    let request = ParquetRequest::new(0)?;
    let (started, ready) = mpsc::channel();
    let (sender, receiver) = mpsc::channel();
    let worker = thread::spawn(move || {
        let _ = started.send(());
        let _ = sender.send(worker_backend.parquet_metadata(source.id, request, worker_cancel));
    });
    ready.recv_timeout(Duration::from_secs(5))?;
    assert!(matches!(
        receiver.recv_timeout(Duration::from_millis(30)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    cancel.cancel();
    assert!(matches!(
        receiver.recv_timeout(Duration::from_secs(5))?,
        Err(QueryError::Cancelled)
    ));
    worker.join().map_err(|_| "Metadata worker panicked")?;
    assert!(data_cancel.check().is_ok());
    drop(cursor);
    assert!(
        backend
            .parquet_metadata(source.id, request, Cancellation::default())
            .is_ok()
    );
    Ok(())
}
