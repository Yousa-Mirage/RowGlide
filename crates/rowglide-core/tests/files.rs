use arrow_array::{Array, Int64Array};
use duckdb::Connection;
use rowglide_app::{Backend, Cancellation, Preview, Query, Sort};
use rowglide_core::DuckDbBackend;
use std::{
    fs,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

/// Verify real CSV registration, duplicate paths, bound filters and global sorting.
///
/// Also checks aggregate SQL, rejection of write/multiple statements and a filter
/// containing SQL-like text that must remain a literal value.
#[test]
fn csv_filters_sorting_sql_and_quoted_paths() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("people's data.csv");
    fs::write(&path, "id,city,amount\n1,London,3\n2,Tokyo,\n3,London,1\n")?;
    let backend = DuckDbBackend::default();
    let source = backend.open(&path, Cancellation::default())?;
    assert_eq!(source.rows, None);
    assert_eq!(backend.open(&path, Cancellation::default())?.id, source.id);
    let cancel = Cancellation::default();
    let preview = Preview::collect(
        backend.query(
            Query {
                source: source.id,
                search: "London".into(),
                sort: Some(Sort {
                    column: 2,
                    descending: false,
                }),
                limit: Some(1),
                ..Query::default()
            },
            cancel.clone(),
        )?,
        &cancel,
    )?;
    assert_eq!(preview.rows, 1);
    let id = preview.batches[0]
        .column(0)
        .as_any()
        .downcast_ref::<Int64Array>()
        .ok_or("Expected integer ID")?;
    assert_eq!(id.value(0), 3);
    let cancel = Cancellation::default();
    let result = Preview::collect(
        backend.query(
            Query {
                source: source.id,
                sql: Some(
                    "SELECT city, count(*) AS n FROM current_file GROUP BY city ORDER BY n DESC;"
                        .into(),
                ),
                ..Query::default()
            },
            cancel.clone(),
        )?,
        &cancel,
    )?;
    assert_eq!(result.rows, 2);
    for sql in [
        "DELETE FROM current_file",
        "SELECT 1; SELECT 2",
        "COPY current_file TO '/tmp/rowglide-unwanted.csv'",
    ] {
        assert!(
            backend
                .query(
                    Query {
                        source: source.id,
                        sql: Some(sql.into()),
                        ..Query::default()
                    },
                    Cancellation::default()
                )
                .is_err()
        );
    }
    let cancel = Cancellation::default();
    let empty = Preview::collect(
        backend.query(
            Query {
                source: source.id,
                search: "' OR true --".into(),
                ..Query::default()
            },
            cancel.clone(),
        )?,
        &cancel,
    )?;
    assert_eq!(empty.rows, 0);
    assert_eq!(empty.schema.fields().len(), 3);
    Ok(())
}

/// Check typed nested Parquet data and global ordering through the legacy preview.
///
/// Asserts the retained row ceiling and truncation flag; printed elapsed time is
/// an informal debug observation rather than a performance acceptance threshold.
#[test]
fn parquet_nested_preview_is_bounded_and_globally_sorted() -> Result<(), Box<dyn std::error::Error>>
{
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("nested.parquet");
    let conn = Connection::open_in_memory()?;
    conn.execute("COPY (SELECT i AS id, [i, i+1] AS items, {'name': 'hello', 'value': i} AS profile FROM range(50000) t(i)) TO ? (FORMAT PARQUET)", [path.to_string_lossy().as_ref()])?;
    let backend = DuckDbBackend::default();
    let source = backend.open(&path, Cancellation::default())?;
    let cancel = Cancellation::default();
    let started = Instant::now();
    let preview = Preview::collect(
        backend.query(
            Query {
                source: source.id,
                sort: Some(Sort {
                    column: 0,
                    descending: true,
                }),
                ..Query::default()
            },
            cancel.clone(),
        )?,
        &cancel,
    )?;
    eprintln!(
        "50k nested Parquet sorted preview: {:?}, {} retained rows",
        started.elapsed(),
        preview.rows
    );
    assert_eq!(preview.rows, 8192);
    assert!(preview.truncated);
    assert!(matches!(
        preview.schema.field(1).data_type(),
        arrow_schema::DataType::List(_)
    ));
    let ids = preview.batches[0]
        .column(0)
        .as_any()
        .downcast_ref::<Int64Array>()
        .ok_or("Expected integer ID")?;
    assert_eq!(ids.value(0), 49999);
    assert!(!ids.is_null(0));
    Ok(())
}

/// Cancel an expensive DuckDB query and verify prompt shutdown plus subsequent recovery.
///
/// A successful query afterward proves that worker cleanup released the shared
/// engine gate instead of leaving later requests blocked.
#[test]
fn cancellation_interrupts_engine_and_releases_worker() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("small.csv");
    fs::write(&path, "id\n1\n")?;
    let backend = Arc::new(DuckDbBackend::default());
    backend.open(&path, Cancellation::default())?;
    let cancel = Cancellation::default();
    let token = cancel.clone();
    let worker_backend = backend.clone();
    let worker = thread::spawn(move || {
        let mut cursor = worker_backend.query(
            Query {
                sql: Some(
                    "SELECT sum(a.i*b.i) FROM range(100000000) a(i), range(100000000) b(i)".into(),
                ),
                ..Query::default()
            },
            token,
        )?;
        cursor.next_batch()
    });
    thread::sleep(Duration::from_millis(100));
    let started = Instant::now();
    cancel.cancel();
    let result = worker.join().map_err(|_| "Query worker panicked")?;
    assert!(result.is_err());
    assert!(started.elapsed() < Duration::from_secs(5));
    let cancel = Cancellation::default();
    assert_eq!(
        Preview::collect(backend.query(Query::default(), cancel.clone())?, &cancel)?.rows,
        1
    );
    Ok(())
}

/// Exercise missing/corrupt files, quoted column sorting and empty-result schemas.
///
/// Also verifies null placement after an explicit offset and schema retention for
/// an empty SQL result.
#[test]
fn corrupt_files_empty_results_and_quoted_columns() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let backend = DuckDbBackend::default();
    assert!(
        backend
            .open(&temp.path().join("missing.csv"), Cancellation::default())
            .is_err()
    );
    let bad = temp.path().join("bad.parquet");
    fs::write(&bad, "not parquet")?;
    assert!(backend.open(&bad, Cancellation::default()).is_err());
    let path = temp.path().join("quotes.csv");
    fs::write(&path, "\"a\"\"b\",id\nLondon,1\nTokyo,2\n,3\n")?;
    let source = backend.open(&path, Cancellation::default())?;
    let cancel = Cancellation::default();
    let preview = Preview::collect(
        backend.query(
            Query {
                source: source.id,
                sort: Some(Sort {
                    column: 0,
                    descending: true,
                }),
                offset: 2,
                ..Query::default()
            },
            cancel.clone(),
        )?,
        &cancel,
    )?;
    assert_eq!(preview.rows, 1);
    assert!(preview.batches[0].column(0).is_null(0));
    let cancel = Cancellation::default();
    let empty = Preview::collect(
        backend.query(
            Query {
                source: source.id,
                sql: Some("SELECT * FROM current_file WHERE false".into()),
                ..Query::default()
            },
            cancel.clone(),
        )?,
        &cancel,
    )?;
    assert_eq!(empty.rows, 0);
    assert_eq!(empty.schema.fields().len(), 2);
    Ok(())
}
