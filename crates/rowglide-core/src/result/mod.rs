//! Memory-first SQL results with shared budgets, incremental spill and asynchronous rereads.

mod budget;
mod disk;
mod store;
#[cfg(test)]
mod tests;

use budget::MemoryCharge;
pub use budget::{CacheLimits, CacheUsage};
pub use store::ResultStore;

use crate::{Backend, Cancellation, Query, QueryError, QuerySession, SessionEvent};
use arrow_array::RecordBatch;
use arrow_schema::SchemaRef;
use async_channel::{Receiver, Sender};
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

/// Resource limits for one retained SQL result, separate from DuckDB's execution budget.
#[derive(Clone, Copy)]
pub struct ResultLimits {
    /// Optional per-result memory ceiling, also subject to the store's shared budget.
    pub memory_bytes: usize,
    /// Optional per-result disk ceiling, also subject to the store's shared budget.
    pub disk_bytes: u64,
    /// Maximum segment-index entries, bounding metadata even for tiny batches.
    pub batches: usize,
}

impl Default for ResultLimits {
    /// Let the shared store govern bytes, while bounding each result to 65,536 segments.
    fn default() -> Self {
        Self {
            memory_bytes: usize::MAX,
            disk_bytes: u64::MAX,
            batches: 65536,
        }
    }
}

/// Explicit execution status; only Complete means all requested rows were generated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResultPhase {
    /// Waiting for schema or continuously producing more rows.
    Running,
    /// Every row was retained in memory or on disk and the query cursor has been released.
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
    /// Number of rows retained in memory or fully written to indexed disk segments.
    pub rows: usize,
    /// Current terminal or running state of the result.
    pub phase: ResultPhase,
    /// Retained cache bytes, excluding transport, decoding and inspector references.
    pub memory_bytes: usize,
    /// Bytes committed to indexed disk segments.
    pub disk_bytes: u64,
}

/// One retained batch's row range and optional location in the private IPC file.
#[derive(Clone, Copy)]
struct Segment {
    /// First zero-based result row in this batch.
    start: usize,
    /// Number of rows in the batch.
    rows: usize,
    /// Whether this batch has a durable IPC copy and can be evicted.
    persisted: bool,
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
    /// Constant-time batch lookup; worker reclamation uses each entry's access stamp.
    cache: HashMap<usize, CachedBatch>,
    /// At most eight queued reads plus one in-progress read, deduplicated by segment index.
    pending: HashSet<usize>,
    /// Shared file lifetime, initialized lazily during worker-driven spill.
    disk: Option<Arc<disk::Disk>>,
    /// A failed read prevents repeated attempts from causing a render/error loop.
    read_failed: bool,
    /// New batches go directly to disk after the first memory-pressure spill.
    disk_mode: bool,
    /// Prevent retries after a partial write; existing memory-only rows remain intact.
    spill_failed: bool,
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
    /// Global coordination and accounting shared by all result handles.
    store: Arc<ResultStore>,
    /// Lazily opened writer; only accessed under the store's worker gate.
    writer: Mutex<Option<(std::fs::File, Arc<disk::Disk>)>>,
    /// Monotonic last-access stamp used to select a result for reclamation.
    touched: AtomicU64,
}

/// An owned batch and its retained-memory reservation; shallow GUI clones are additional.
struct CachedBatch {
    /// Typed arrays shared with the table and inspector on demand.
    batch: RecordBatch,
    /// Dropping the entry releases its contribution to the global budget.
    charge: MemoryCharge,
    /// Logical last access used for batch eviction within a result.
    touched: u64,
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
    /// Pass the same store for all tabs to enforce one shared budget.
    /// Returns an error for invalid budgets or thread startup; production failures
    /// appear in snapshots. The caller's token controls generation, not cache reads.
    pub fn start(
        store: Arc<ResultStore>,
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
                cache: HashMap::new(),
                pending: HashSet::new(),
                disk: None,
                read_failed: false,
                disk_mode: false,
                spill_failed: false,
            }),
            updates: updates_tx,
            cancel,
            shutdown: Cancellation::default(),
            limits,
            touched: AtomicU64::new(store.tick()),
            store: store.clone(),
            writer: Mutex::new(None),
        });
        store.register(&shared);
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
        self.shared
            .touched
            .store(self.shared.store.tick(), Ordering::Relaxed);
        let mut state = self.shared.state();
        let index = state
            .segments
            .partition_point(|segment| segment.start <= row)
            .checked_sub(1)?;
        let segment = state.segments[index];
        if row >= segment.start + segment.rows {
            return None;
        }
        if let Some(entry) = state.cache.get_mut(&index) {
            entry.touched = self.shared.store.tick();
            return Some((entry.batch.clone(), row - segment.start));
        }
        if segment.persisted
            && !state.read_failed
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

/// Pull continuously with bounded transport, pausing consumption during cache reclamation.
/// Only fully retained batches become visible; storage failure preserves earlier rows.
fn write_result(
    shared: &Shared,
    backend: Arc<dyn Backend>,
    query: Query,
) -> Result<(), QueryError> {
    let session = QuerySession::start(backend, query, shared.cancel.clone())?;
    loop {
        shared.cancel.check()?;
        match session.next_blocking()? {
            SessionEvent::Schema(schema) => {
                shared.state().snapshot.schema = Some(schema);
                shared.notify();
            }
            SessionEvent::Batch(batch) => {
                if batch.num_rows() != 0 {
                    shared.store.append(shared, batch)?;
                    shared.notify();
                }
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
        let result = shared.store.read(&shared, index);
        if shared.shutdown.check().is_err() {
            break;
        }
        let mut state = shared.state();
        state.pending.remove(&index);
        if let Err(error) = result {
            state.read_failed = true;
            state.snapshot.phase = ResultPhase::Failed(error.to_string());
            shared.cancel.cancel();
        }
        drop(state);
        shared.notify();
    }
}
