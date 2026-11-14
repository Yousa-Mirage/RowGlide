//! Event-driven subscriptions, bounded cache replies and cancellation regressions.

use super::{CacheLimits, QueryResult, ResultLimits, ResultPhase, ResultStore, tests::finished};
use crate::{Cancellation, MockBackend, Query, QueryError};
use arrow_array::Int64Array;
use std::{
    future::Future,
    pin::pin,
    sync::{Arc, mpsc},
    task::{Context, Poll, Waker},
    thread,
    time::Duration,
};

/// Bound async tests with an independent watchdog; the production code uses no polling.
fn wait<F: Future>(future: F) -> Result<F::Output, QueryError> {
    let stop = Cancellation::default();
    let expired = stop.clone();
    let (done, receiver) = mpsc::channel::<()>();
    let watchdog = thread::spawn(move || {
        if matches!(
            receiver.recv_timeout(Duration::from_secs(5)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ) {
            expired.cancel();
        }
    });
    let result = futures_lite::future::block_on(stop.until_cancelled(future));
    drop(done);
    watchdog
        .join()
        .map_err(|_| QueryError::Backend("Test watchdog panicked".into()))?;
    result
}

/// Complete a memory-constrained result and choose a persisted uncached row.
fn spilled() -> Result<(QueryResult, usize, usize), QueryError> {
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
    let (index, row) = {
        let state = result.shared.state();
        state
            .segments
            .iter()
            .enumerate()
            .find(|(index, segment)| segment.persisted && !state.cache.contains_key(index))
            .map(|(index, segment)| (index, segment.start))
            .ok_or_else(|| QueryError::Backend("Expected spilled batch".into()))?
    };
    Ok((result, index, row))
}

/// Subscribers cannot consume one another's updates; cache-only changes and disposal wake both.
#[test]
fn independent_subscribers_observe_cache_changes_and_close() -> Result<(), QueryError> {
    let (result, _, _) = spilled()?;
    let mut first = result.subscribe();
    let mut second = result.subscribe();
    let rows = result.snapshot().rows;
    result.shared.notify();
    assert!(wait(first.changed())?);
    assert!(wait(second.changed())?);
    result.shared.notify();
    result.shared.notify();
    assert!(wait(first.changed())?);
    assert!(wait(second.changed())?);
    assert_eq!(result.snapshot().rows, rows);
    drop(result);
    assert!(!wait(first.changed())?);
    assert!(!wait(second.changed())?);
    Ok(())
}

/// Duplicate requests share a read; cancelling one caller leaves the other and query intact.
#[test]
fn duplicate_read_cancel_does_not_cancel_other_waiters() -> Result<(), QueryError> {
    let (result, index, row) = spilled()?;
    let store = result.shared.store.clone();
    let blocked = store.pause_io();
    let cancel = Cancellation::default();
    let keep = Cancellation::default();
    let mut first = pin!(result.row_async(row, &cancel));
    let mut second = pin!(result.row_async(row, &keep));
    let mut cx = Context::from_waker(Waker::noop());
    assert!(first.as_mut().poll(&mut cx).is_pending());
    assert!(second.as_mut().poll(&mut cx).is_pending());
    {
        let state = result.shared.state();
        assert_eq!(state.pending.len(), 1);
        assert_eq!(state.pending[&index].replies.len(), 2);
    }
    cancel.cancel();
    assert!(matches!(
        first.as_mut().poll(&mut cx),
        Poll::Ready(Err(QueryError::Cancelled))
    ));
    assert_eq!(result.snapshot().phase, ResultPhase::Complete);
    drop(blocked);
    let (batch, local) = wait(second)??;
    let values = batch
        .column(0)
        .as_any()
        .downcast_ref::<Int64Array>()
        .ok_or_else(|| QueryError::Backend("Missing IDs".into()))?;
    assert_eq!(values.value(local), 10001 + row as i64);
    // A reply owns its buffers even after cache eviction.
    let mut state = result.shared.state();
    if let Some(entry) = state.cache.remove(&index) {
        state.snapshot.memory_bytes -= entry.charge.bytes;
    }
    drop(state);
    assert_eq!(values.value(local), 10001 + row as i64);
    Ok(())
}

/// Explicit readers are capped, and dropping a wait releases capacity immediately.
#[test]
fn read_waiters_are_bounded_and_dropped_waits_release_slots() -> Result<(), QueryError> {
    let (result, index, row) = spilled()?;
    let store = result.shared.store.clone();
    let blocked = store.pause_io();
    let cancel = Cancellation::default();
    let mut waits = (0..8)
        .map(|_| Box::pin(result.row_async(row, &cancel)))
        .collect::<Vec<_>>();
    let mut cx = Context::from_waker(Waker::noop());
    for future in &mut waits {
        assert!(future.as_mut().poll(&mut cx).is_pending());
    }
    assert!(matches!(
        wait(result.row_async(row, &cancel))?,
        Err(QueryError::Invalid(_))
    ));
    waits.pop();
    let mut replacement = pin!(result.row_async(row, &cancel));
    assert!(replacement.as_mut().poll(&mut cx).is_pending());
    assert_eq!(result.shared.state().pending[&index].replies.len(), 8);
    drop(waits);
    cancel.cancel();
    assert!(matches!(wait(replacement)?, Err(QueryError::Cancelled)));
    drop(blocked);
    assert_eq!(result.read_waiters.available_permits(), 8);
    Ok(())
}

/// A disk failure completes all pending replies and later reads retain the typed error source.
#[test]
fn read_failure_finishes_every_waiter() -> Result<(), QueryError> {
    let (result, _, row) = spilled()?;
    let store = result.shared.store.clone();
    let blocked = store.pause_io();
    let path = result
        .shared
        .state()
        .disk
        .as_ref()
        .map(|disk| disk.path())
        .ok_or_else(|| QueryError::Backend("Missing disk cache".into()))?;
    std::fs::OpenOptions::new()
        .write(true)
        .open(path)?
        .set_len(0)?;
    let cancel = Cancellation::default();
    let mut first = pin!(result.row_async(row, &cancel));
    let mut second = pin!(result.row_async(row, &cancel));
    let mut cx = Context::from_waker(Waker::noop());
    assert!(first.as_mut().poll(&mut cx).is_pending());
    assert!(second.as_mut().poll(&mut cx).is_pending());
    drop(blocked);
    for outcome in [
        wait(first)?,
        wait(second)?,
        wait(result.row_async(row, &cancel))?,
    ] {
        let Err(QueryError::CacheRead(error)) = outcome else {
            return Err(QueryError::Backend("Expected shared read failure".into()));
        };
        assert!(std::error::Error::source(error.as_ref()).is_some());
    }
    assert!(matches!(result.snapshot().phase, ResultPhase::Failed(_)));
    Ok(())
}

/// Shutdown wakes outstanding waits before blocked storage has returned.
#[test]
fn shutdown_finishes_awaiting_read() -> Result<(), QueryError> {
    let (result, _, row) = spilled()?;
    let store = result.shared.store.clone();
    let blocked = store.pause_io();
    let cancel = Cancellation::default();
    let mut future = pin!(result.row_async(row, &cancel));
    assert!(
        future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending()
    );
    result.shared.shutdown.cancel();
    assert!(matches!(wait(future)?, Err(QueryError::Cancelled)));
    drop(blocked);
    Ok(())
}

/// Cancelling while the bounded request queue is full releases the waiting slot and reservation.
#[test]
fn cancellation_while_read_queue_is_full() -> Result<(), QueryError> {
    let (result, _, row) = spilled()?;
    let reserved = result
        .reads
        .try_reserve_many(8)
        .map_err(|_| QueryError::Backend("Expected empty request queue".into()))?;
    let cancel = Cancellation::default();
    let mut future = pin!(result.row_async(row, &cancel));
    let mut cx = Context::from_waker(Waker::noop());
    assert!(future.as_mut().poll(&mut cx).is_pending());
    assert!(result.shared.state().pending.is_empty());
    cancel.cancel();
    assert!(matches!(
        future.as_mut().poll(&mut cx),
        Poll::Ready(Err(QueryError::Cancelled))
    ));
    assert_eq!(result.read_waiters.available_permits(), 8);
    drop(reserved);
    assert_eq!(result.reads.capacity(), 8);
    // Execution cancellation must not prevent reading already-retained results.
    result.cancel();
    assert!(wait(result.row_async(row, &Cancellation::default()))?.is_ok());
    assert!(matches!(
        wait(result.row_async(usize::MAX, &Cancellation::default()))?,
        Err(QueryError::Invalid(_))
    ));
    Ok(())
}
