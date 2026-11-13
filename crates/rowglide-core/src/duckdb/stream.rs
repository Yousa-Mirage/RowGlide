//! Bounded Arrow transport; cursor ownership controls worker lifetime.

use super::{
    engine::{self, FileSource},
    sql,
};
use crate::{BatchCursor, Cancellation, Query, QueryError};
use arrow_array::RecordBatch;
use arrow_schema::SchemaRef;
use duckdb::{Connection, params_from_iter};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

/// Owns a cancellation-monitor thread for one DuckDB connection.
///
/// Once cancellation is requested, repeated interrupts cover races between query
/// preparation and execution. Dropping the monitor stops and joins its thread.
pub(super) struct Interrupt {
    /// Separate monitor-shutdown flag; does not change the request cancellation token.
    stop: Arc<AtomicBool>,
    /// Owned monitor thread, taken and joined exactly once during drop.
    worker: Option<JoinHandle<()>>,
}

impl Interrupt {
    /// Start a monitor that checks cancellation and interrupts the connection every 10 ms.
    ///
    /// The monitor owns an interrupt handle, not the connection. Returns a backend
    /// error if its thread cannot be created; its lifetime should stay within that of
    /// the query's connection.
    pub(super) fn new(conn: &Connection, cancel: Cancellation) -> Result<Self, QueryError> {
        let handle = conn.interrupt_handle();
        let stop = Arc::new(AtomicBool::new(false));
        let done = stop.clone();
        let worker = thread::Builder::new()
            .name("duckdb-cancel".into())
            .spawn(move || {
                while !done.load(Ordering::Acquire) {
                    if cancel.check().is_err() {
                        handle.interrupt();
                    }
                    thread::sleep(Duration::from_millis(10));
                }
            })?;
        Ok(Self {
            stop,
            worker: Some(worker),
        })
    }
}

impl Drop for Interrupt {
    /// Signal the interrupt monitor to stop and wait for its thread to finish.
    ///
    /// A thread panic is ignored during cleanup. Joining may briefly block, so the
    /// monitor is owned by background query work.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Worker-to-cursor protocol: schema first, followed by batches and a terminal event.
///
/// An explicit End distinguishes successful exhaustion from an unexpected channel
/// disconnection. Error carries execution or transport failures to the consumer.
enum Message {
    /// Initial result schema, sent before any row batches.
    Schema(SchemaRef),
    /// One owned typed batch handed from producer to consumer.
    Batch(RecordBatch),
    /// Successful query exhaustion; distinct from channel disconnection.
    End,
    /// Terminal query failure, including normalized cancellation errors.
    Error(QueryError),
}

/// Owns the receiving end of one query stream and the worker's lifetime.
///
/// Creation consumes the initial schema message. Subsequent pulls block for typed
/// batches; dropping the cursor cancels execution and joins the worker.
struct Cursor {
    /// Schema retained after the startup handshake consumes the Schema message.
    schema: SchemaRef,
    /// Removed before joining so a producer blocked on send can exit.
    receiver: Option<Receiver<Message>>,
    /// Shared token checked by receiving code and the query's interrupt monitor.
    cancel: Cancellation,
    /// Producer thread whose connection and spill directory live until it exits.
    worker: Option<JoinHandle<()>>,
    /// Set after an End or Error message; later pulls return no additional batch.
    ended: bool,
}

impl Cursor {
    /// Wait for one protocol message, checking cancellation every 20 ms.
    ///
    /// Returns cancellation when the token is set or the receiver was removed. An
    /// unexpected disconnect is a backend error, never successful EOF.
    fn receive(&self) -> Result<Message, QueryError> {
        let receiver = self.receiver.as_ref().ok_or(QueryError::Cancelled)?;
        loop {
            self.cancel.check()?;
            match receiver.recv_timeout(Duration::from_millis(20)) {
                Ok(message) => return Ok(message),
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(QueryError::Backend(
                        "Query worker ended without a completion message".into(),
                    ));
                }
            }
        }
    }
}

impl BatchCursor for Cursor {
    /// Clone the result schema received during startup, including for empty results.
    fn schema(&self) -> SchemaRef {
        self.schema.clone()
    }
    /// Return an unknown total because this streaming path does not run COUNT.
    fn total_rows(&self) -> Option<usize> {
        None
    }
    /// Receive the next Arrow batch or terminal event from the query worker.
    ///
    /// End returns `None`; a worker error is propagated once and also marks the cursor
    /// ended. Later pulls on an ended cursor return `None`, so consumers must retain
    /// the original error rather than treating a retry as successful completion.
    fn next_batch(&mut self) -> Result<Option<RecordBatch>, QueryError> {
        if self.ended {
            return Ok(None);
        }
        match self.receive()? {
            Message::Batch(batch) => Ok(Some(batch)),
            Message::End => {
                self.ended = true;
                Ok(None)
            }
            Message::Error(error) => {
                self.ended = true;
                Err(error)
            }
            Message::Schema(_) => Err(QueryError::Backend("Unexpected result schema".into())),
        }
    }
}

impl Drop for Cursor {
    /// Cancel unfinished execution, disconnect the receiver and join the query worker.
    /// Successful EOF must not set the caller's cancellation token.
    ///
    /// Disconnecting before joining releases a producer blocked on the bounded channel.
    /// Joining may block until DuckDB responds to interruption; drop this cursor off
    /// the GUI thread. Worker panics are ignored during cleanup.
    fn drop(&mut self) {
        if !self.ended {
            self.cancel.cancel();
        }
        // Disconnect first so a producer blocked by backpressure can exit.
        self.receiver.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Spawn a query worker and wait for its first schema or error message.
///
/// A capacity-one channel provides backpressure between production and consumption.
/// Returns an owned cursor only after schema arrival; thread creation, cancellation
/// and startup failures propagate after cleanup. The caller must run in background
/// because waiting for schema and dropping a failed cursor can block.
pub(super) fn start(
    file: FileSource,
    query: Query,
    cancel: Cancellation,
    gate: Arc<Mutex<()>>,
) -> Result<Box<dyn BatchCursor>, QueryError> {
    let (sender, receiver) = mpsc::sync_channel(1);
    let token = cancel.clone();
    let worker = thread::Builder::new()
        .name("duckdb-query".into())
        .spawn(move || {
            if let Err(error) = produce(file, query, token.clone(), gate, &sender) {
                let error = if token.check().is_err() {
                    QueryError::Cancelled
                } else {
                    error
                };
                let _ = sender.send(Message::Error(error));
            }
        })?;
    let mut cursor = Cursor {
        schema: Arc::new(arrow_schema::Schema::empty()),
        receiver: Some(receiver),
        cancel,
        worker: Some(worker),
        ended: false,
    };
    match cursor.receive()? {
        Message::Schema(schema) => {
            cursor.schema = schema;
            Ok(Box::new(cursor))
        }
        Message::Error(error) => Err(error),
        _ => Err(QueryError::Backend("Query did not provide a schema".into())),
    }
}

/// Run one DuckDB query under the engine gate and send its Arrow result stream.
///
/// Owns the connection, interrupt monitor and private spill directory for the entire
/// stream. Sends schema before batches, rejects any batch over 16 MiB and sends End
/// only on successful exhaustion. A disconnected batch receiver stops production;
/// other failures are returned to `start` for error delivery. Bounded sends may
/// block until the consumer advances or drops its cursor.
fn produce(
    file: FileSource,
    query: Query,
    cancel: Cancellation,
    gate: Arc<Mutex<()>>,
    sender: &SyncSender<Message>,
) -> Result<(), QueryError> {
    let _guard = engine::lock_engine(&gate, &cancel)?;
    let temp = tempfile::tempdir()?;
    let conn = engine::connection(temp.path())?;
    let _interrupt = Interrupt::new(&conn, cancel.clone())?;
    engine::register(&conn, &file.path, file.info.format)?;
    let (sql, params) = sql::build(&query, &file.schema)?;
    cancel.check()?;
    let mut statement = conn.prepare(&sql)?;
    let batches = statement.stream_arrow(params_from_iter(params.iter()))?;
    send(sender, Message::Schema(batches.get_schema()))?;
    for batch in batches {
        cancel.check()?;
        let batch = batch?;
        // Avoid retaining a single pathological batch beyond the preview budget.
        if batch.get_array_memory_size() > 16 * 1024 * 1024 {
            return Err(QueryError::Invalid(
                "A result batch exceeds the 16 MiB memory budget; reduce Limit or select smaller values".into(),
            ));
        }
        send(sender, Message::Batch(batch))?;
    }
    send(sender, Message::End)
}

/// Send one protocol message with backpressure, treating a departed consumer as cancellation.
/// Do not retain the failed send's payload; dropping it releases any Arrow buffers.
fn send(sender: &SyncSender<Message>, message: Message) -> Result<(), QueryError> {
    sender.send(message).map_err(|_| QueryError::Cancelled)
}

#[cfg(test)]
mod tests {
    use super::{Cursor, Message, send};
    use crate::{BatchCursor, Cancellation, QueryError};
    use arrow_array::RecordBatch;
    use arrow_schema::Schema;
    use std::sync::{Arc, mpsc};

    /// Losing the consumer is cancellation at every send stage, including schema and EOF.
    #[test]
    fn departed_consumer_cancels_all_send_stages() {
        let schema = Arc::new(Schema::empty());
        for message in [
            Message::Schema(schema.clone()),
            Message::Batch(RecordBatch::new_empty(schema)),
            Message::End,
        ] {
            let (sender, receiver) = mpsc::sync_channel(1);
            drop(receiver);
            assert!(matches!(send(&sender, message), Err(QueryError::Cancelled)));
        }
    }

    /// Losing the producer without a terminal event is a failure, never successful EOF.
    #[test]
    fn departed_producer_is_not_completion() {
        let (sender, receiver) = mpsc::sync_channel(1);
        drop(sender);
        let mut cursor = Cursor {
            schema: Arc::new(Schema::empty()),
            receiver: Some(receiver),
            cancel: Cancellation::default(),
            worker: None,
            ended: false,
        };
        assert!(matches!(cursor.next_batch(), Err(QueryError::Backend(_))));
    }
}
