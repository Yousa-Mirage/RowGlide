//! Real Parquet footer inspection, typed metadata paging and error boundaries.

use arrow_schema::DataType;
use duckdb::Connection;
use rowglide_core::{Backend, Cancellation, DuckDbBackend, QueryError};
use std::{error::Error, fs};

/// Inspect nested schema and exact footer totals independently of query results.
#[test]
fn logical_schema_and_footer_summary() -> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("nested ' data.parquet");
    let conn = Connection::open_in_memory()?;
    conn.execute("COPY (SELECT i AS id, [i, NULL] AS items, [{'list': i, 'element': i}] AS root FROM range(5000) t(i)) TO ? (FORMAT PARQUET, COMPRESSION ZSTD, ROW_GROUP_SIZE 2048)", [path.to_string_lossy().as_ref()])?;
    let backend = DuckDbBackend::default();
    let source = backend.open(&path, Cancellation::default())?;
    assert_eq!(source.rows, Some(5000));
    let summary = source.parquet.ok_or("Missing summary")?;
    assert_eq!(summary.columns, 3);
    assert_eq!(summary.file_bytes, fs::metadata(&path)?.len());
    assert!(summary.created_by.is_some());
    let schema = backend.parquet_metadata(source.id, Cancellation::default())?;
    let names = schema
        .iter()
        .map(|row| row.field.name().as_str())
        .collect::<Vec<_>>();
    // List encoding wrappers are absent; genuine struct children remain visible.
    assert_eq!(names, ["id", "items", "root", "list", "element"]);
    let depths: Vec<_> = schema.iter().map(|r| r.depth).collect();
    assert_eq!(depths, [0, 0, 0, 1, 1]);
    let DataType::List(element) = schema[1].field.data_type() else {
        return Err("Expected a logical list".into());
    };
    assert_eq!(element.data_type(), &DataType::Int64);
    assert!(element.is_nullable());
    Ok(())
}

/// Load wide file schemas without dropping or duplicating nodes; empty files retain columns.
#[test]
fn complete_wide_schema_for_empty_file() -> Result<(), Box<dyn Error>> {
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
    let fields = backend.parquet_metadata(source.id, Cancellation::default())?;
    let names: Vec<_> = fields.iter().map(|row| row.field.name().clone()).collect();
    assert_eq!(names, (0..205).map(|i| format!("c{i}")).collect::<Vec<_>>());
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
    let cancel = Cancellation::default();
    cancel.cancel();
    assert!(matches!(
        backend.parquet_metadata(source.id, cancel),
        Err(QueryError::Cancelled)
    ));
    assert!(
        backend
            .parquet_metadata(999, Cancellation::default())
            .is_err()
    );
    let csv = temp.path().join("plain.csv");
    fs::write(&csv, "id\n1\n")?;
    let csv = backend.open(&csv, Cancellation::default())?;
    assert!(
        backend
            .parquet_metadata(csv.id, Cancellation::default())
            .is_err()
    );
    fs::remove_file(path)?;
    assert!(
        backend
            .parquet_metadata(source.id, Cancellation::default())
            .is_err()
    );
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
    let (started, ready) = mpsc::channel();
    let (sender, receiver) = mpsc::channel();
    let worker = thread::spawn(move || {
        let _ = started.send(());
        let _ = sender.send(worker_backend.parquet_metadata(source.id, worker_cancel));
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
            .parquet_metadata(source.id, Cancellation::default())
            .is_ok()
    );
    Ok(())
}

/// Required lists can contain nullable items and nullable lists can require their items.
#[test]
fn list_nullability_is_independent_of_element_nullability() -> Result<(), Box<dyn Error>> {
    use arrow_schema::{Field, Schema};
    use parquet::arrow::ArrowWriter;
    use std::sync::Arc;

    let temp = tempfile::tempdir()?;
    let path = temp.path().join("declared.parquet");
    let schema = Schema::new(
        [false, true]
            .into_iter()
            .map(|optional| {
                Field::new(
                    if optional { "optional" } else { "required" },
                    DataType::List(Field::new("element", DataType::Utf8, !optional).into()),
                    optional,
                )
            })
            .collect::<Vec<_>>(),
    );
    ArrowWriter::try_new(fs::File::create(&path)?, Arc::new(schema), None)?.close()?;
    let backend = DuckDbBackend::default();
    let source = backend.open(&path, Cancellation::default())?;
    let page = backend.parquet_metadata(source.id, Cancellation::default())?;
    assert_eq!(page.len(), 2);
    for (row, optional) in page.iter().zip([false, true]) {
        assert_eq!(row.field.is_nullable(), optional);
        let DataType::List(element) = row.field.data_type() else {
            return Err("Expected a logical list".into());
        };
        assert_eq!(element.is_nullable(), !optional);
    }
    Ok(())
}
