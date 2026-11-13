//! Complete query result spooling with bounded memory, disk and index retention.

mod disk;

use crate::{Backend, Cancellation, Query, QueryError, QuerySession, SessionEvent};
use arrow_array::RecordBatch;
use arrow_schema::SchemaRef;
use async_channel::{Receiver, Sender};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{Arc, Mutex, MutexGuard, mpsc},
    thread,
    time::Duration,
};

/// Resource limits for one retained SQL result, separate from DuckDB's execution budget.
#[derive(Clone, Copy)]
pub struct ResultLimits {
    /// Maximum cached Arrow buffer bytes; external inspector references are additional.
    pub memory_bytes: usize,
    /// Maximum IPC file bytes, including per-segment schema overhead.
    pub disk_bytes: u64,
    /// Maximum segment-index entries, bounding metadata even for tiny batches.
    pub batches: usize,
}

impl Default for ResultLimits {
    /// Keep 64 MiB of Arrow buffers and at most 2 GiB / 65,536 segments on disk.
    fn default() -> Self {
        Self {
            memory_bytes: 64 * 1024 * 1024,
            disk_bytes: 2 * 1024 * 1024 * 1024,
            batches: 65536,
        }
    }
}

/// Explicit execution status; only Complete means all requested rows were generated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResultPhase {
    /// Waiting for schema or continuously producing more rows.
    Running,
    /// Every row was committed to disk and the query cursor has been released.
    Complete,
    /// User interrupted execution; retained rows are a partial result.
    Cancelled,
    /// Query, storage or cache read failed; retained rows must not be presented as complete.
    Failed(String),
}

/// Cheap snapshot used by UI status and row-count updates without disk access.
#[derive(Clone)]
pub struct ResultSnapshot {
    /// Result schema once startup has produced it, including for empty results.
    pub schema: Option<SchemaRef>,
    /// Number of rows fully written to indexed segments in the temporary result file.
    pub rows: usize,
    /// Current terminal or running state of the result.
    pub phase: ResultPhase,
    /// Retained cache bytes, excluding transport, decoding and inspector references.
    pub memory_bytes: usize,
    /// Bytes committed to indexed disk segments.
    pub disk_bytes: u64,
}

/// One committed batch's row range and byte range within the private IPC file.
#[derive(Clone, Copy)]
struct Segment {
    /// First zero-based result row in this batch.
    start: usize,
    /// Number of rows in the batch.
    rows: usize,
    /// Starting IPC stream offset.
    offset: u64,
    /// Length of the independent IPC stream.
    length: u64,
}

/// Mutable metadata and bounded LRU cache; no file I/O occurs while this lock is held.
struct State {
    /// Latest schema, counts and execution state.
    snapshot: ResultSnapshot,
    /// Increasing row ranges; binary search locates a batch for any generated row.
    segments: Vec<Segment>,
    /// Least recently used batches at the front, most recent at the back.
    cache: VecDeque<(usize, RecordBatch)>,
    /// At most eight queued reads plus one in-progress read, deduplicated by segment index.
    pending: HashSet<usize>,
    /// Shared file lifetime, initialized by the writer worker.
    disk: Option<Arc<disk::Disk>>,
    /// A failed read prevents repeated attempts from causing a render/error loop.
    read_failed: bool,
}

/// Worker-shared state that outlives the UI handle until background cleanup finishes.
struct Shared {
    /// Short critical sections around metadata and cache references.
    state: Mutex<State>,
    /// Coalesced notifications; a capacity-one channel cannot accumulate progress events.
    updates: Sender<()>,
    /// Execution cancellation; partial results remain readable afterward.
    cancel: Cancellation,
    /// Separate lifetime token for stopping cache reads when the result is released.
    shutdown: Cancellation,
    /// Immutable per-result resource budgets.
    limits: ResultLimits,
}

impl Shared {
    /// Lock metadata, recovering its value if a previous worker panicked while holding it.
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }

    /// Wake the UI once; a pending wake already covers newer metadata changes.
    fn notify(&self) {
        let _ = self.updates.try_send(());
    }

    /// Publish a terminal state without overwriting an earlier cancel or storage failure.
    fn finish(&self, phase: ResultPhase) {
        let mut state = self.state();
        if state.snapshot.phase == ResultPhase::Running {
            state.snapshot.phase = phase;
        }
        drop(state);
        self.notify();
    }

    /// Retain a decoded batch within the memory budget by evicting least-recent entries.
    fn cache(&self, state: &mut State, index: usize, batch: RecordBatch) -> Result<(), QueryError> {
        let bytes = buffer_bytes(&batch);
        if bytes > self.limits.memory_bytes {
            return Err(disk::error(
                "A batch exceeds the memory cache budget; select smaller values",
            ));
        }
        while state.snapshot.memory_bytes.saturating_add(bytes) > self.limits.memory_bytes {
            if let Some((_, old)) = state.cache.pop_front() {
                state.snapshot.memory_bytes -= buffer_bytes(&old);
            } else {
                break;
            }
        }
        state.snapshot.memory_bytes += bytes;
        state.cache.push_back((index, batch));
        Ok(())
    }
}

/// Readable SQL result whose writer runs continuously independently of scrolling.
///
/// Cloning an Arc around this handle preserves the file. Dropping its last owner
/// cancels generation and stops cache reads without joining the GUI thread. Disk
/// deletion occurs after workers release their references. Queries that fail or
/// are cancelled retain already committed rows with an explicit incomplete status.
pub struct QueryResult {
    /// Metadata and worker state retained until all operations have finished.
    shared: Arc<Shared>,
    /// GUI-facing coalesced update receiver; one active consumer per result.
    updates: Receiver<()>,
    /// Bounded nonblocking requests to the dedicated disk reader.
    reads: mpsc::SyncSender<usize>,
}

impl QueryResult {
    /// Start writer/reader workers without performing file I/O on the caller's thread.
    /// Returns an error for invalid budgets or thread startup; production failures
    /// appear in snapshots. The caller's token controls generation, not cache reads.
    pub fn start(
        backend: Arc<dyn Backend>,
        query: Query,
        cancel: Cancellation,
        limits: ResultLimits,
    ) -> Result<Self, QueryError> {
        if limits.memory_bytes == 0 || limits.disk_bytes == 0 || limits.batches == 0 {
            return Err(disk::error("Result budgets must be positive"));
        }
        let (updates_tx, updates) = async_channel::bounded(1);
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                snapshot: ResultSnapshot {
                    schema: None,
                    rows: 0,
                    phase: ResultPhase::Running,
                    memory_bytes: 0,
                    disk_bytes: 0,
                },
                segments: Vec::new(),
                cache: VecDeque::new(),
                pending: HashSet::new(),
                disk: None,
                read_failed: false,
            }),
            updates: updates_tx,
            cancel,
            shutdown: Cancellation::default(),
            limits,
        });
        let (reads, receiver) = mpsc::sync_channel(8);
        let result = Self {
            shared: shared.clone(),
            updates,
            reads,
        };
        let reader = shared.clone();
        thread::Builder::new()
            .name("result-reader".into())
            .spawn(move || read_worker(reader, receiver))?;
        thread::Builder::new()
            .name("result-writer".into())
            .spawn(move || {
                let outcome = write_result(&shared, backend, query);
                let phase = match outcome {
                    Ok(()) => ResultPhase::Complete,
                    Err(QueryError::Cancelled) => ResultPhase::Cancelled,
                    Err(error) => ResultPhase::Failed(error.to_string()),
                };
                shared.finish(phase);
            })?;
        Ok(result)
    }

    /// Clone lightweight progress metadata without waiting for disk or query execution.
    pub fn snapshot(&self) -> ResultSnapshot {
        self.shared.state().snapshot.clone()
    }

    /// Await a coalesced progress/cache notification on the existing GUI executor.
    /// Continues after execution finishes because scrolling can still request disk reads.
    pub async fn changed(&self) {
        let _ = self.updates.recv().await;
    }

    /// Cancel further generation while retaining committed rows for inspection.
    pub fn cancel(&self) {
        self.shared.cancel.cancel();
        self.shared.finish(ResultPhase::Cancelled);
    }

    /// Obtain a shared cached batch and batch-local row, or enqueue its asynchronous read.
    /// Never reads disk during rendering. The bounded queue deduplicates pending batches;
    /// a later render retries if the queue is full. Returned arrays may outlive eviction.
    pub fn row(&self, row: usize) -> Option<(RecordBatch, usize)> {
        let mut state = self.shared.state();
        let index = state
            .segments
            .partition_point(|segment| segment.start <= row)
            .checked_sub(1)?;
        let segment = state.segments[index];
        if row >= segment.start + segment.rows {
            return None;
        }
        if let Some(position) = state.cache.iter().position(|(id, _)| *id == index) {
            let entry = state.cache.remove(position)?;
            let batch = entry.1.clone();
            state.cache.push_back(entry);
            return Some((batch, row - segment.start));
        }
        if !state.read_failed
            && !state.pending.contains(&index)
            && self.reads.try_send(index).is_ok()
        {
            state.pending.insert(index);
        }
        None
    }
}

impl Drop for QueryResult {
    /// Stop background generation/read work; workers retain the file until they exit.
    fn drop(&mut self) {
        self.shared.cancel.cancel();
        self.shared.shutdown.cancel();
        self.updates.close();
    }
}

/// Count unique backing allocations within a batch, including nested/null buffers.
/// IPC columns often share one allocation; Arrow's per-array size sum counts that
/// allocation repeatedly. Separate cached batches are conservatively charged separately.
fn buffer_bytes(batch: &RecordBatch) -> usize {
    let mut allocations = HashMap::<usize, usize>::new();
    let mut arrays = batch
        .columns()
        .iter()
        .map(|array| array.to_data())
        .collect::<Vec<_>>();
    let mut metadata = 0usize;
    while let Some(data) = arrays.pop() {
        metadata = metadata.saturating_add(size_of_val(&data));
        for buffer in data
            .buffers()
            .iter()
            .chain(data.nulls().map(|nulls| nulls.buffer()))
        {
            let bytes = buffer
                .capacity()
                .max(buffer.ptr_offset().saturating_add(buffer.len()));
            let entry = allocations
                .entry(buffer.data_ptr().as_ptr() as usize)
                .or_default();
            *entry = (*entry).max(bytes);
        }
        arrays.extend(data.child_data().iter().cloned());
    }
    allocations
        .values()
        .fold(metadata, |sum, bytes| sum.saturating_add(*bytes))
}

/// Spool every delivered batch, publishing row ranges only after complete segment writes.
/// The first batch is cached immediately; later batches stay on disk until visible.
fn write_result(
    shared: &Shared,
    backend: Arc<dyn Backend>,
    query: Query,
) -> Result<(), QueryError> {
    let (disk, mut file) = disk::Disk::create()?;
    shared.state().disk = Some(disk);
    let session = QuerySession::start(backend, query, shared.cancel.clone())?;
    loop {
        shared.cancel.check()?;
        match session.next_blocking()? {
            SessionEvent::Schema(schema) => {
                shared.state().snapshot.schema = Some(schema);
                shared.notify();
            }
            SessionEvent::Batch(batch) => {
                if batch.num_rows() == 0 {
                    continue;
                }
                if buffer_bytes(&batch) > shared.limits.memory_bytes {
                    return Err(disk::error(
                        "A batch exceeds the memory cache budget; select smaller values",
                    ));
                }
                let (count, offset, start) = {
                    let state = shared.state();
                    (
                        state.segments.len(),
                        state.snapshot.disk_bytes,
                        state.snapshot.rows,
                    )
                };
                if count >= shared.limits.batches {
                    return Err(disk::error(
                        "Result exceeds the batch index budget; narrow the query or add LIMIT",
                    ));
                }
                let rows = start
                    .checked_add(batch.num_rows())
                    .ok_or_else(|| disk::error("Result row count overflow"))?;
                let length = disk::append(&mut file, &batch, shared.limits.disk_bytes - offset)?;
                shared.cancel.check()?;
                let mut state = shared.state();
                state.segments.push(Segment {
                    start,
                    rows: batch.num_rows(),
                    offset,
                    length,
                });
                state.snapshot.rows = rows;
                state.snapshot.disk_bytes += length;
                if count == 0 {
                    shared.cache(&mut state, count, batch)?;
                }
                drop(state);
                shared.notify();
            }
            SessionEvent::Complete => return Ok(()),
            SessionEvent::Failed(error) => return Err(error),
        }
    }
}

/// Decode requested committed batches outside the metadata lock and wake the UI afterward.
fn read_worker(shared: Arc<Shared>, receiver: mpsc::Receiver<usize>) {
    while shared.shutdown.check().is_ok() {
        let index = match receiver.recv_timeout(Duration::from_millis(20)) {
            Ok(index) => index,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        let source = {
            let state = shared.state();
            state.disk.clone().zip(state.segments.get(index).copied())
        };
        let Some((disk, segment)) = source else {
            continue;
        };
        let result = disk.read(segment.offset, segment.length);
        if shared.shutdown.check().is_err() {
            break;
        }
        let mut state = shared.state();
        state.pending.remove(&index);
        let result = result.and_then(|batch| shared.cache(&mut state, index, batch));
        if let Err(error) = result {
            state.read_failed = true;
            state.snapshot.phase = ResultPhase::Failed(error.to_string());
            shared.cancel.cancel();
        }
        drop(state);
        shared.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MockBackend;
    use arrow_array::Int64Array;
    use std::time::Instant;

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
            Arc::new(MockBackend),
            Query::default(),
            Cancellation::default(),
            ResultLimits::default(),
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
        assert!(result.row(0).is_some());
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
            Arc::new(MockBackend),
            Query::default(),
            Cancellation::default(),
            ResultLimits::default(),
        )?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while result.snapshot().rows == 0 {
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
}
