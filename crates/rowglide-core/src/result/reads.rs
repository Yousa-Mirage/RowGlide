//! Bounded explicit cache reads and typed completion replies shared by duplicate requests.

use super::{QueryResult, Shared};
use crate::{Cancellation, QueryError};
use arrow_array::RecordBatch;
use std::sync::Arc;
use tokio::sync::oneshot;

/// One batch request, shared between visible-cell prefetch and explicit awaiting readers.
pub(super) struct PendingRead {
    /// Rendering requested this batch even if every explicit waiter has departed.
    pub(super) visible: bool,
    /// At most eight live explicit readers per result; closed replies are pruned on insertion.
    pub(super) replies: Vec<oneshot::Sender<Result<RecordBatch, Arc<QueryError>>>>,
}

impl QueryResult {
    /// Await one already-generated row, returning shared Arrow buffers and its batch-local index.
    /// Uses bounded requests and replies, never polling or disk I/O on the calling thread.
    /// At most eight explicit cache readers may wait per result; excess calls fail clearly.
    /// Cancellation affects this read only, leaving the query and other waiters intact.
    /// Failed/cancelled queries remain readable wherever their retained data is available.
    pub async fn row_async(
        &self,
        row: usize,
        cancel: &Cancellation,
    ) -> Result<(RecordBatch, usize), QueryError> {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(QueryError::Cancelled),
            _ = self.shared.shutdown.cancelled() => Err(QueryError::Cancelled),
            value = self.read_row(row) => value,
        }
    }

    /// Resolve or register one explicit read. The outer future handles cancellation.
    /// A queue permit is reserved outside metadata locks, then state is checked again
    /// before registration so concurrent cache fills cannot produce a lost wakeup.
    async fn read_row(&self, row: usize) -> Result<(RecordBatch, usize), QueryError> {
        let (index, local) = {
            let mut state = self.shared.state();
            let index = state
                .segments
                .partition_point(|s| s.start <= row)
                .checked_sub(1)
                .ok_or_else(|| QueryError::Invalid("Result row is unavailable".into()))?;
            let segment = state.segments[index];
            if row >= segment.start + segment.rows {
                return Err(QueryError::Invalid("Result row is unavailable".into()));
            }
            if let Some(entry) = state.cache.get_mut(&index) {
                entry.touched = self.shared.store.tick();
                return Ok((entry.batch.clone(), row - segment.start));
            }
            if let Some(error) = &state.read_error {
                return Err(QueryError::CacheRead(error.clone()));
            }
            (index, row - segment.start)
        };
        let _slot = self
            .read_waiters
            .try_acquire()
            .map_err(|_| QueryError::Invalid("Too many pending cache reads; retry later".into()))?;
        let (reply, receive) = oneshot::channel();
        let mut reply = Some(reply);
        {
            let mut state = self.shared.state();
            if let Some(pending) = state.pending.get_mut(&index) {
                pending.replies.retain(|sender| !sender.is_closed());
                pending.replies.extend(reply.take());
            }
        }
        if let Some(reply) = reply {
            let permit = self
                .reads
                .reserve()
                .await
                .map_err(|_| QueryError::Backend("Cache reader stopped".into()))?;
            let mut state = self.shared.state();
            if let Some(entry) = state.cache.get_mut(&index) {
                entry.touched = self.shared.store.tick();
                return Ok((entry.batch.clone(), local));
            }
            if let Some(error) = &state.read_error {
                return Err(QueryError::CacheRead(error.clone()));
            }
            let pending = state.pending.entry(index).or_insert_with(|| {
                permit.send(index);
                PendingRead {
                    visible: false,
                    replies: Vec::new(),
                }
            });
            pending.replies.retain(|sender| !sender.is_closed());
            pending.replies.push(reply);
        }
        let batch = receive
            .await
            .map_err(|_| {
                QueryError::Backend("Cache reader ended without completing the read".into())
            })?
            .map_err(QueryError::CacheRead)?;
        self.shared.touched.store(
            self.shared.store.tick(),
            std::sync::atomic::Ordering::Relaxed,
        );
        Ok((batch, local))
    }
}

/// Read once and deliver the same owned outcome to all waiters, then notify all observers.
/// Explicit-only requests abandoned before processing are skipped; visible-cell requests remain useful.
pub(super) fn complete(shared: &Shared, index: usize) {
    {
        let mut state = shared.state();
        let Some(pending) = state.pending.get(&index) else {
            return;
        };
        if !pending.visible && pending.replies.iter().all(oneshot::Sender::is_closed) {
            state.pending.remove(&index);
            return;
        }
    }
    match shared.store.read(shared, index) {
        Ok(batch) => {
            let pending = shared.state().pending.remove(&index);
            if let Some(pending) = pending {
                for reply in pending.replies {
                    let _ = reply.send(Ok(batch.clone()));
                }
            }
        }
        Err(error) => {
            let error = Arc::new(error);
            {
                let mut state = shared.state();
                state.read_error = Some(error.clone());
                state.snapshot.phase = super::ResultPhase::Failed(error.to_string());
            }
            shared.cancel.cancel();
            fail_pending(shared, error);
        }
    }
    shared.notify();
}

/// Finish every queued waiter on read failure or shutdown without retaining Arrow payloads.
pub(super) fn fail_pending(shared: &Shared, error: Arc<QueryError>) {
    let pending = std::mem::take(&mut shared.state().pending);
    for pending in pending.into_values() {
        for reply in pending.replies {
            let _ = reply.send(Err(error.clone()));
        }
    }
}
