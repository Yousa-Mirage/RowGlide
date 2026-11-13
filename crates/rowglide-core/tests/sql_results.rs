//! Real DuckDB SQL semantics and typed disk-cache recovery beyond the old preview limit.

use arrow_array::Int64Array;
use rowglide_core::{
    Backend, Cancellation, DuckDbBackend, Query, QueryError, QueryResult, ResultLimits, ResultPhase,
};
use std::{
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

/// Wait for completion or surface the real query/storage error with a bounded deadline.
fn finish(result: &QueryResult) -> Result<(), QueryError> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match result.snapshot().phase {
            ResultPhase::Complete => return Ok(()),
            ResultPhase::Failed(error) => return Err(QueryError::Backend(error)),
            ResultPhase::Cancelled => return Err(QueryError::Cancelled),
            ResultPhase::Running => {}
        }
        if Instant::now() > deadline {
            return Err(QueryError::Backend("Timed out".into()));
        }
        thread::sleep(Duration::from_millis(5));
    }
}

/// SQL returns all 20,000 rows, respects only its own LIMIT/OFFSET and keeps empty schemas.
#[test]
fn sql_semantics_and_nested_recovery() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("source.csv");
    std::fs::write(&path, "id\n1\n")?;
    let backend = Arc::new(DuckDbBackend::default());
    backend.open(&path, Cancellation::default())?;
    for (suffix, expected, first) in [
        ("", 20000, 0),
        ("LIMIT 12000 OFFSET 100", 12000, 100),
        ("LIMIT 0", 0, 0),
    ] {
        let query = Query {
            sql: Some(format!(
                "SELECT i AS id, [i, NULL] AS items, {{'value': i}} AS profile FROM range(20000) t(i) ORDER BY i {suffix}"
            )),
            limit: Some(1),
            offset: 999999,
            ..Query::default()
        };
        let result = QueryResult::start(
            backend.clone(),
            query,
            Cancellation::default(),
            ResultLimits {
                memory_bytes: 1024 * 1024,
                ..ResultLimits::default()
            },
        )?;
        finish(&result)?;
        let snapshot = result.snapshot();
        assert_eq!(snapshot.rows, expected);
        assert_eq!(snapshot.schema.ok_or("Missing schema")?.fields().len(), 3);
        if expected == 0 {
            continue;
        }
        for row in [expected - 1, 0] {
            let deadline = Instant::now() + Duration::from_secs(5);
            let (batch, local) = loop {
                if let Some(value) = result.row(row) {
                    break value;
                }
                if Instant::now() > deadline {
                    return Err("Cache read timed out".into());
                }
                thread::sleep(Duration::from_millis(5));
            };
            let ids = batch
                .column(0)
                .as_any()
                .downcast_ref::<Int64Array>()
                .ok_or("Missing IDs")?;
            assert_eq!(ids.value(local), (first + row) as i64);
            assert!(matches!(
                batch.schema().field(1).data_type(),
                arrow_schema::DataType::List(_)
            ));
            assert!(result.snapshot().memory_bytes <= 1024 * 1024);
        }
    }
    Ok(())
}

/// Cancel an executing SQL aggregate, then confirm the next query can acquire the engine gate.
#[test]
fn cancel_running_sql_then_recover() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("source.csv");
    std::fs::write(&path, "id\n1\n")?;
    let backend = Arc::new(DuckDbBackend::default());
    backend.open(&path, Cancellation::default())?;
    let result = QueryResult::start(
        backend.clone(),
        Query {
            sql: Some(
                "SELECT sum(a.i*b.i) FROM range(100000000) a(i), range(100000000) b(i)".into(),
            ),
            ..Query::default()
        },
        Cancellation::default(),
        ResultLimits::default(),
    )?;
    thread::sleep(Duration::from_millis(100));
    result.cancel();
    assert_eq!(result.snapshot().phase, ResultPhase::Cancelled);
    drop(result);
    let next = QueryResult::start(
        backend,
        Query {
            sql: Some("SELECT * FROM current_file".into()),
            ..Query::default()
        },
        Cancellation::default(),
        ResultLimits::default(),
    )?;
    finish(&next)?;
    assert_eq!(next.snapshot().rows, 1);
    Ok(())
}
