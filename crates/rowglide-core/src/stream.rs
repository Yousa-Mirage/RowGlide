//! Bounded Arrow transport; cursor ownership controls worker lifetime.

use crate::{
    engine::{self, FileSource},
    sql,
};
use arrow_array::RecordBatch;
use arrow_schema::SchemaRef;
use duckdb::{Connection, params_from_iter};
use rowglide_app::{BatchCursor, Cancellation, Query, QueryError};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

/// Repeated interrupts cover the race between prepare and execute.
pub(crate) struct Interrupt {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl Interrupt {
    pub(crate) fn new(conn: &Connection, cancel: Cancellation) -> Result<Self, QueryError> {
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
            })
            .map_err(engine::error)?;
        Ok(Self {
            stop,
            worker: Some(worker),
        })
    }
}

impl Drop for Interrupt {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

enum Message {
    Schema(SchemaRef),
    Batch(RecordBatch),
    End,
    Error(QueryError),
}

struct Cursor {
    schema: SchemaRef,
    receiver: Option<Receiver<Message>>,
    cancel: Cancellation,
    worker: Option<JoinHandle<()>>,
    ended: bool,
}

impl Cursor {
    fn receive(&self) -> Result<Message, QueryError> {
        let receiver = self.receiver.as_ref().ok_or(QueryError::Cancelled)?;
        loop {
            self.cancel.check()?;
            match receiver.recv_timeout(Duration::from_millis(20)) {
                Ok(message) => return Ok(message),
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(engine::error(
                        "Query worker ended without a completion message",
                    ));
                }
            }
        }
    }
}

impl BatchCursor for Cursor {
    fn schema(&self) -> SchemaRef {
        self.schema.clone()
    }
    fn total_rows(&self) -> Option<usize> {
        None
    }
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
            Message::Schema(_) => Err(engine::error("Unexpected result schema")),
        }
    }
}

impl Drop for Cursor {
    fn drop(&mut self) {
        self.cancel.cancel();
        // Disconnect first so a producer blocked by backpressure can exit.
        self.receiver.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

pub(crate) fn start(
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
        })
        .map_err(engine::error)?;
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
        _ => Err(engine::error("Query did not provide a schema")),
    }
}

fn produce(
    file: FileSource,
    query: Query,
    cancel: Cancellation,
    gate: Arc<Mutex<()>>,
    sender: &SyncSender<Message>,
) -> Result<(), QueryError> {
    let _guard = engine::lock_engine(&gate, &cancel)?;
    let temp = tempfile::tempdir().map_err(engine::error)?;
    let conn = engine::connection(temp.path())?;
    let _interrupt = Interrupt::new(&conn, cancel.clone())?;
    engine::register(&conn, &file.path, file.info.format)?;
    let (sql, params) = sql::build(&query, &file.schema)?;
    cancel.check()?;
    let mut statement = conn.prepare(&sql).map_err(engine::error)?;
    let batches = statement
        .stream_arrow(params_from_iter(params.iter()))
        .map_err(engine::error)?;
    sender
        .send(Message::Schema(batches.get_schema()))
        .map_err(engine::error)?;
    for batch in batches {
        cancel.check()?;
        let batch = batch.map_err(engine::error)?;
        // Avoid retaining a single pathological batch beyond the preview budget.
        if batch.get_array_memory_size() > 16 * 1024 * 1024 {
            return Err(engine::error(
                "A result batch exceeds the 16 MiB memory budget; reduce Limit or select smaller values",
            ));
        }
        if sender.send(Message::Batch(batch)).is_err() {
            return Ok(());
        }
    }
    sender.send(Message::End).map_err(engine::error)
}
