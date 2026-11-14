//! Memory-first retention, shared-budget spill and asynchronous recovery regressions.

use super::*;
use crate::MockBackend;
use arrow_array::Int64Array;
use std::time::{Duration, Instant};

/// Fixture whose cursor emits one batch and then waits for cancellation.
struct PausedBackend;

/// A single-batch cursor that keeps the query running until its token is cancelled.
struct PausedCursor {
    /// Typed fixture data, emitted once before blocking.
    batch: Option<RecordBatch>,
    /// Schema remains available after the batch is delivered.
    schema: SchemaRef,
    /// Cancellation releases the blocked producer without a separate test signal.
    cancel: Cancellation,
}

impl Backend for PausedBackend {
    /// This fixture creates query data directly and has no registered files.
    fn sources(&self) -> Vec<crate::Source> {
        Vec::new()
    }

    /// Create one typed row followed by an intentionally unfinished stream.
    fn query(
        &self,
        _: Query,
        cancel: Cancellation,
    ) -> Result<Box<dyn crate::BatchCursor>, QueryError> {
        let batch = RecordBatch::try_from_iter([(
            "id",
            Arc::new(Int64Array::from(vec![42])) as arrow_array::ArrayRef,
        )])?;
        Ok(Box::new(PausedCursor {
            schema: batch.schema(),
            batch: Some(batch),
            cancel,
        }))
    }
}

impl crate::BatchCursor for PausedCursor {
    /// Return the fixture schema independently of stream progress.
    fn schema(&self) -> SchemaRef {
        self.schema.clone()
    }

    /// The deliberately unfinished stream does not advertise a total.
    fn total_rows(&self) -> Option<usize> {
        None
    }

    /// Emit one batch, then cooperatively block until the test cancels execution.
    fn next_batch(&mut self) -> Result<Option<RecordBatch>, QueryError> {
        if let Some(batch) = self.batch.take() {
            return Ok(Some(batch));
        }
        loop {
            self.cancel.check()?;
            thread::sleep(Duration::from_millis(1));
        }
    }
}

/// A committed first batch is readable while the producer is still blocked on more data.
#[test]
fn rows_are_available_before_completion() -> Result<(), QueryError> {
    let result = QueryResult::start(
        ResultStore::new(CacheLimits::default())?,
        Arc::new(PausedBackend),
        Query::default(),
        Cancellation::default(),
        ResultLimits::default(),
    )?;
    let (batch, local) = row(&result, 0)?;
    assert_eq!(batch.num_rows(), 1);
    assert_eq!(local, 0);
    assert_eq!(result.snapshot().phase, ResultPhase::Running);
    result.cancel();
    assert_eq!(result.snapshot().phase, ResultPhase::Cancelled);
    assert!(result.row(0).is_some());
    Ok(())
}

/// A damaged disk segment changes a completed result to an explicit read failure.
#[test]
fn cache_read_failure_is_not_success() -> Result<(), QueryError> {
    let result = QueryResult::start(
        ResultStore::new(CacheLimits::default())?,
        Arc::new(MockBackend),
        Query::default(),
        Cancellation::default(),
        ResultLimits {
            memory_bytes: 128 * 1024,
            ..ResultLimits::default()
        },
    )?;
    assert_eq!(finished(&result)?.phase, ResultPhase::Complete);
    let path = result
        .shared
        .state()
        .disk
        .as_ref()
        .map(|disk| disk.path())
        .ok_or_else(|| disk::error("Missing file"))?;
    std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .and_then(|file| file.set_len(0))?;
    assert!(row(&result, 49999).is_err());
    assert!(matches!(result.snapshot().phase, ResultPhase::Failed(_)));

    Ok(())
}

/// Wait for a terminal phase with a deadline so worker regressions fail instead of hang.
fn finished(result: &QueryResult) -> Result<ResultSnapshot, QueryError> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let snapshot = result.snapshot();
        if snapshot.phase != ResultPhase::Running {
            return Ok(snapshot);
        }
        if Instant::now() > deadline {
            return Err(disk::error("Timed out waiting for result"));
        }
        thread::sleep(Duration::from_millis(5));
    }
}

/// Wait for a requested cached row while allowing the dedicated reader to restore it.
fn row(result: &QueryResult, row: usize) -> Result<(RecordBatch, usize), QueryError> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(value) = result.row(row) {
            return Ok(value);
        }
        if let ResultPhase::Failed(message) = result.snapshot().phase {
            return Err(disk::error(message));
        }
        if Instant::now() > deadline {
            return Err(disk::error(format!("Timed out reading row {row}")));
        }
        thread::sleep(Duration::from_millis(5));
    }
}

/// Evicted first/middle/last rows recover with intact nested types within the memory cap.
#[test]
fn bounded_cache_round_trip() -> Result<(), QueryError> {
    let memory = 128 * 1024;
    let result = QueryResult::start(
        ResultStore::new(CacheLimits::default())?,
        Arc::new(MockBackend),
        Query::default(),
        Cancellation::default(),
        ResultLimits {
            memory_bytes: memory,
            ..ResultLimits::default()
        },
    )?;
    let snapshot = finished(&result)?;
    assert_eq!(snapshot.phase, ResultPhase::Complete);
    assert_eq!(snapshot.rows, 50000);
    assert!(snapshot.disk_bytes > memory as u64);
    for index in [0, 49999, 25000, 0] {
        let (batch, local) = row(&result, index)?;
        let ids = batch
            .column(0)
            .as_any()
            .downcast_ref::<Int64Array>()
            .ok_or_else(|| disk::error("Missing IDs"))?;
        assert_eq!(ids.value(local), index as i64 + 10001);
        assert!(matches!(
            batch.schema().field(5).data_type(),
            arrow_schema::DataType::List(_)
        ));
        assert!(result.snapshot().memory_bytes <= memory);
    }
    assert!(result.row(50000).is_none());
    Ok(())
}

/// Disk, index and memory exhaustion remain explicit failures, never successful truncation.
#[test]
fn budgets_fail_explicitly() -> Result<(), QueryError> {
    for limits in [
        ResultLimits {
            disk_bytes: 100,
            memory_bytes: 128 * 1024,
            ..ResultLimits::default()
        },
        ResultLimits {
            batches: 1,
            ..ResultLimits::default()
        },
        ResultLimits {
            memory_bytes: 1,
            ..ResultLimits::default()
        },
    ] {
        let result = QueryResult::start(
            ResultStore::new(CacheLimits::default())?,
            Arc::new(MockBackend),
            Query::default(),
            Cancellation::default(),
            limits,
        )?;
        let snapshot = finished(&result)?;
        assert!(matches!(snapshot.phase, ResultPhase::Failed(_)));
        assert!(snapshot.memory_bytes <= limits.memory_bytes);
        assert!(snapshot.disk_bytes <= limits.disk_bytes);
    }
    Ok(())
}

/// Cancellation keeps a partial status and cached data; dropping the handle releases its file.
#[test]
fn cancellation_and_cleanup() -> Result<(), QueryError> {
    let result = QueryResult::start(
        ResultStore::new(CacheLimits::default())?,
        Arc::new(MockBackend),
        Query::default(),
        Cancellation::default(),
        ResultLimits {
            memory_bytes: 128 * 1024,
            ..ResultLimits::default()
        },
    )?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while result.snapshot().disk_bytes == 0 {
        if Instant::now() > deadline {
            return Err(disk::error("No first batch"));
        }
        thread::sleep(Duration::from_millis(1));
    }
    let path = result
        .shared
        .state()
        .disk
        .as_ref()
        .map(|disk| disk.path())
        .ok_or_else(|| disk::error("Missing file"))?;
    result.cancel();
    assert!(matches!(
        result.snapshot().phase,
        ResultPhase::Cancelled | ResultPhase::Complete
    ));
    let _ = row(&result, 0)?;
    drop(result);
    while path.exists() {
        if Instant::now() > deadline {
            return Err(disk::error("Temporary result file leaked"));
        }
        thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

/// Start a deterministic one-batch result using the supplied application-wide budget.
fn small_result(store: Arc<ResultStore>) -> Result<QueryResult, QueryError> {
    QueryResult::start(
        store,
        Arc::new(MockBackend),
        Query {
            limit: Some(100),
            ..Query::default()
        },
        Cancellation::default(),
        ResultLimits::default(),
    )
}

/// Measure the real fixture allocation instead of depending on Arrow allocator details.
fn small_batch_bytes() -> Result<usize, QueryError> {
    let mut cursor = MockBackend.query(
        Query {
            limit: Some(100),
            ..Query::default()
        },
        Cancellation::default(),
    )?;
    let batch = cursor
        .next_batch()?
        .ok_or_else(|| disk::error("Missing fixture batch"))?;
    Ok(buffer_bytes(&batch))
}

/// Wait for asynchronous worker and file ownership to release every shared resource charge.
fn released(store: &ResultStore) -> Result<(), QueryError> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while store.usage()
        != (CacheUsage {
            memory_bytes: 0,
            disk_bytes: 0,
        })
    {
        if Instant::now() > deadline {
            return Err(disk::error("Cache charges leaked"));
        }
        thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

/// Empty and small results never create disk storage, and owned memory is released on drop.
#[test]
fn memory_first_without_temporary_files() -> Result<(), QueryError> {
    let store = ResultStore::new(CacheLimits::default())?;
    for limit in [0, 100] {
        let result = QueryResult::start(
            store.clone(),
            Arc::new(MockBackend),
            Query {
                limit: Some(limit),
                ..Query::default()
            },
            Cancellation::default(),
            ResultLimits::default(),
        )?;
        assert_eq!(finished(&result)?.phase, ResultPhase::Complete);
        assert_eq!(result.snapshot().rows, limit);
        assert!(result.snapshot().schema.is_some());
        assert!(result.shared.state().disk.is_none());
        assert_eq!(store.usage().disk_bytes, 0);
        if limit > 0 {
            assert!(result.row(0).is_some());
        }
        drop(result);
        released(&store)?;
    }
    Ok(())
}

/// Pressure spills the least-recently-used completed result and keeps every row recoverable.
#[test]
fn shared_budget_spills_cold_completed_results() -> Result<(), QueryError> {
    let bytes = small_batch_bytes()?;
    let store = ResultStore::new(CacheLimits {
        memory_bytes: bytes * 2,
        ..CacheLimits::default()
    })?;
    let first = small_result(store.clone())?;
    assert_eq!(finished(&first)?.phase, ResultPhase::Complete);
    let second = small_result(store.clone())?;
    assert_eq!(finished(&second)?.phase, ResultPhase::Complete);
    assert!(first.row(0).is_some());
    let third = small_result(store.clone())?;
    assert_eq!(finished(&third)?.phase, ResultPhase::Complete);
    assert_eq!(first.snapshot().disk_bytes, 0);
    assert!(second.snapshot().disk_bytes > 0);
    assert_eq!(third.snapshot().disk_bytes, 0);
    for result in [&second, &first, &third] {
        let (batch, local) = row(result, 99)?;
        let ids = batch
            .column(0)
            .as_any()
            .downcast_ref::<Int64Array>()
            .ok_or_else(|| disk::error("Missing IDs"))?;
        assert_eq!(ids.value(local), 10100);
        assert_eq!(result.snapshot().phase, ResultPhase::Complete);
        assert!(store.usage().memory_bytes <= bytes * 2);
    }
    drop((first, second, third));
    released(&store)
}

/// Failed spill preserves the older result and accounts for partial writes until deletion.
#[test]
fn global_disk_failure_preserves_memory_rows() -> Result<(), QueryError> {
    let bytes = small_batch_bytes()?;
    let store = ResultStore::new(CacheLimits {
        memory_bytes: bytes,
        disk_bytes: 128,
    })?;
    let first = small_result(store.clone())?;
    assert_eq!(finished(&first)?.phase, ResultPhase::Complete);
    let second = small_result(store.clone())?;
    assert!(matches!(finished(&second)?.phase, ResultPhase::Failed(_)));
    assert_eq!(first.snapshot().phase, ResultPhase::Complete);
    assert!(first.row(99).is_some());
    assert_eq!(store.usage().memory_bytes, bytes);
    let path = first
        .shared
        .state()
        .disk
        .as_ref()
        .map(|disk| disk.path())
        .ok_or_else(|| disk::error("Missing partial file"))?;
    assert_eq!(store.usage().disk_bytes, std::fs::metadata(path)?.len());
    assert!(store.usage().disk_bytes > 0 && store.usage().disk_bytes <= 128);
    drop((first, second));
    released(&store)
}

/// Concurrent producers share one limit and release all memory/file charges after shutdown.
#[test]
fn concurrent_results_share_budget_and_cleanup() -> Result<(), QueryError> {
    let store = ResultStore::new(CacheLimits {
        memory_bytes: 256 * 1024,
        ..CacheLimits::default()
    })?;
    let mut results = Vec::new();
    for _ in 0..2 {
        results.push(QueryResult::start(
            store.clone(),
            Arc::new(MockBackend),
            Query::default(),
            Cancellation::default(),
            ResultLimits::default(),
        )?);
    }
    for result in &results {
        assert_eq!(finished(result)?.phase, ResultPhase::Complete);
        assert_eq!(result.snapshot().rows, 50000);
        let _ = row(result, 49999)?;
        assert!(store.usage().memory_bytes <= 256 * 1024);
    }
    assert_eq!(
        store.usage().memory_bytes,
        results
            .iter()
            .map(|r| r.snapshot().memory_bytes)
            .sum::<usize>()
    );
    drop(results);
    released(&store)
}

/// Closing a result wakes an idle disk reader and releases its shared state without polling.
#[test]
fn dropping_result_releases_idle_reader() -> Result<(), QueryError> {
    let result = QueryResult::start(
        ResultStore::new(CacheLimits::default())?,
        Arc::new(MockBackend),
        Query {
            source: 2,
            ..Query::default()
        },
        Cancellation::default(),
        ResultLimits::default(),
    )?;
    assert_eq!(finished(&result)?.phase, ResultPhase::Complete);
    let shared = Arc::downgrade(&result.shared);
    drop(result);
    let deadline = Instant::now() + Duration::from_secs(5);
    while shared.upgrade().is_some() {
        if Instant::now() >= deadline {
            return Err(disk::error(
                "Idle result reader retained shared state after drop",
            ));
        }
        thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}
