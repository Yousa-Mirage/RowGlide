//! Local-file registry and resource-limited DuckDB connections.

use super::{freshness::FileStamp, parquet, stream};
use crate::{Backend, BatchCursor, Cancellation, Query, QueryError, Source};
use arrow_schema::SchemaRef;
use duckdb::{Config, Connection};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};

use tokio::sync::{Semaphore, SemaphorePermit};

/// Registered local file metadata plus the schema obtained during inspection.
///
/// The canonical path identifies duplicate registrations. The schema is a snapshot;
/// queries reopen the file, so later external modifications are not isolated.
#[derive(Clone)]
pub(super) struct FileSource {
    /// Stable registry handle and display metadata returned to callers.
    pub(super) info: Source,
    /// Canonical local path reused for each independent query scan.
    pub(super) path: PathBuf,
    /// Source schema captured when the file was first registered.
    pub(super) schema: SchemaRef,
    /// Metadata baseline captured before schema inspection.
    stamp: FileStamp,
    /// Shared with running workers; a detected change requires explicit reload.
    changed: Arc<AtomicBool>,
}

impl FileSource {
    /// Compare filesystem metadata without the engine gate, latching any failure or change.
    pub(super) fn changed(&self) -> bool {
        if FileStamp::read(&self.path).map_or(true, |stamp| stamp != self.stamp) {
            self.changed.store(true, Ordering::Relaxed);
        }
        self.changed.load(Ordering::Relaxed)
    }

    /// Reject reads of a changed source. This is a best-effort check, not a file lock.
    pub(super) fn ensure_current(&self) -> Result<(), QueryError> {
        if self.changed() {
            Err(QueryError::SourceChanged)
        } else {
            Ok(())
        }
    }
}

/// Local-file registry and serialized DuckDB query backend.
///
/// Registration and execution share one engine gate, limiting concurrent engine
/// budgets. Every query opens its own in-memory connection and scans the source
/// file directly; the registry holds neither result buffers nor live GUI objects.
pub struct DuckDbBackend {
    /// Stable source slots; closed entries are empty and IDs are never reused.
    files: Mutex<Vec<Option<FileSource>>>,
    /// Serializes inspection and execution so requests cannot multiply engine budgets.
    gate: Arc<Semaphore>,
}

impl Default for DuckDbBackend {
    /// Create an empty registry with one fair, asynchronously awaited engine permit.
    fn default() -> Self {
        Self {
            files: Mutex::new(Vec::new()),
            gate: Arc::new(Semaphore::new(1)),
        }
    }
}

impl DuckDbBackend {
    /// Inspect a stable file under the engine gate and atomically register or replace it.
    /// Reload failures preserve the prior descriptor, including its latched change flag.
    fn inspect(
        &self,
        path: &Path,
        cancel: Cancellation,
        replacing: Option<usize>,
    ) -> Result<Source, QueryError> {
        cancel.check()?;
        let path = if replacing.is_some() {
            path.to_owned()
        } else {
            path.canonicalize()?
        };
        if !path.is_file() {
            return Err(QueryError::Invalid(
                "Select a regular CSV or Parquet file".into(),
            ));
        }
        let format = match path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("parquet" | "pq") => "PARQUET",
            Some("csv") => "CSV",
            _ => {
                return Err(QueryError::Invalid(
                    "Supported file types: .parquet, .pq, .csv".into(),
                ));
            }
        };
        if let Some(file) = self
            .registry()?
            .iter()
            .flatten()
            .find(|file| file.path == path && replacing != Some(file.info.id))
        {
            return Ok(file.info.clone());
        }
        let _guard = lock_engine(&self.gate, &cancel)?;
        let stamp = FileStamp::read(&path)?;
        let temp = crate::temporary::Directory::create()?;
        let conn = connection(temp.path())?;
        let _interrupt = stream::Interrupt::new(&conn, cancel.clone())?;
        register(&conn, &path, format)?;
        let mut statement = conn.prepare("SELECT * FROM current_file LIMIT 0")?;
        let schema = statement.stream_arrow([])?.get_schema();
        drop(statement);
        let (rows, parquet) = if format == "PARQUET" {
            let (rows, summary) = parquet::summary(&conn, schema.fields().len())?;
            (Some(rows), Some(summary))
        } else {
            (None, None)
        };
        cancel.check()?;
        if FileStamp::read(&path)? != stamp {
            return Err(QueryError::SourceChanged);
        }
        let mut files = self.registry()?;
        if replacing.is_some_and(|id| files.get(id).is_none_or(Option::is_none)) {
            return Err(QueryError::Invalid(
                "File was closed while reloading".into(),
            ));
        }
        if let Some(file) = files
            .iter()
            .flatten()
            .find(|file| file.path == path && replacing != Some(file.info.id))
        {
            return Ok(file.info.clone());
        }
        let info = Source {
            id: replacing.unwrap_or(files.len()),
            name: path.file_name().map_or_else(
                || path.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            ),
            format,
            rows,
            parquet,
            description: "Local file",
        };
        let file = Some(FileSource {
            info: info.clone(),
            path,
            schema,
            stamp,
            changed: Arc::new(AtomicBool::new(false)),
        });
        if let Some(id) = replacing {
            if let Some(old) = &files[id] {
                old.changed.store(true, Ordering::Relaxed);
            }
            files[id] = file;
        } else {
            files.push(file);
        }
        Ok(info)
    }

    /// Lock the file registry, reporting poison without retaining a borrowed lock guard.
    /// Typed I/O and engine errors bypass this boundary and propagate unchanged.
    fn registry(&self) -> Result<MutexGuard<'_, Vec<Option<FileSource>>>, QueryError> {
        self.files
            .lock()
            .map_err(|_| QueryError::Backend("File registry lock was poisoned".into()))
    }
}

/// Open an in-memory DuckDB connection with bounded execution resources.
///
/// Uses up to four threads, 512 MB engine memory and a 2 GB spill budget under
/// `temp`. Preserves source insertion order and disables automatic extension
/// loading/installing. The caller owns and cleans up the temporary directory.
/// Configuration or connection failures are returned as backend errors.
pub(super) fn connection(temp: &Path) -> Result<Connection, QueryError> {
    let threads = thread::available_parallelism().map_or(2, |n| n.get().min(4));
    let config = Config::default()
        .max_memory("512MB")?
        .threads(threads as i64)?
        .enable_autoload_extension(false)?
        .with("autoinstall_known_extensions", "false")?
        .with("temp_directory", temp.to_string_lossy())?
        .with("max_temp_directory_size", "2GB")?
        .with("preserve_insertion_order", "true")?;
    Ok(Connection::open_in_memory_with_flags(config)?)
}

/// Expose a local file as the connection-local `current_file` view.
///
/// Binds the UTF-8 path through a DuckDB variable rather than SQL interpolation.
/// `PARQUET` selects the Parquet reader; registry-validated CSV sources use bounded
/// automatic inference. Returns errors for non-UTF-8 paths or reader/view setup.
pub(super) fn register(conn: &Connection, path: &Path, format: &str) -> Result<(), QueryError> {
    let path = path
        .to_str()
        .ok_or_else(|| QueryError::Invalid("File path must be valid UTF-8".into()))?;
    conn.execute("SET VARIABLE source_path = ?", [path])?;
    if format == "PARQUET" {
        parquet::register(conn)
    } else {
        Ok(conn.execute_batch("CREATE VIEW current_file AS SELECT * FROM read_csv(getvariable('source_path'), sample_size=20480)")?)
    }
}

/// Await FIFO engine admission or cancellation without opening a connection while queued.
/// A cancellation observed alongside acquisition wins, and drops any acquired permit.
/// All inspection, metadata and query execution use the same single permit.
async fn acquire_engine<'a>(
    gate: &'a Semaphore,
    cancel: &Cancellation,
) -> Result<SemaphorePermit<'a>, QueryError> {
    let permit = cancel
        .until_cancelled(gate.acquire())
        .await?
        .map_err(|_| QueryError::Backend("Engine admission queue closed".into()))?;
    cancel.check()?;
    Ok(permit)
}

/// Bridge the synchronous backend contract to event-driven engine admission.
/// Call only from existing background workers: the thread parks while Tokio's
/// semaphore awaits a permit or cancellation; no timer or Tokio runtime is needed.
pub(super) fn lock_engine<'a>(
    gate: &'a Semaphore,
    cancel: &Cancellation,
) -> Result<SemaphorePermit<'a>, QueryError> {
    futures_lite::future::block_on(acquire_engine(gate, cancel))
}

impl Backend for DuckDbBackend {
    /// Clone the registered-source metadata under the registry lock.
    ///
    /// Returns an empty snapshot if the registry mutex is poisoned; no file inspection
    /// or query execution is performed here.
    fn sources(&self) -> Vec<Source> {
        self.files.lock().map_or_else(
            |_| vec![],
            |files| {
                files
                    .iter()
                    .flatten()
                    .map(|file| file.info.clone())
                    .collect()
            },
        )
    }

    /// Canonicalize, validate and inspect a CSV/Parquet path before registering it.
    ///
    /// Duplicate canonical paths reuse their existing ID. New files are inspected under
    /// the engine gate using a zero-row query to obtain schema without counting rows.
    /// The registry is checked again before insertion to handle concurrent opens.
    /// Errors include unsupported formats, missing files, cancellation and DuckDB
    /// inspection failures. Parquet totals come from the footer; CSV totals remain unknown.
    fn open(&self, path: &Path, cancel: Cancellation) -> Result<Source, QueryError> {
        self.inspect(path, cancel, None)
    }

    /// Check the registered fingerprint without blocking on DuckDB execution.
    fn source_changed(&self, source: usize) -> Result<bool, QueryError> {
        let file = self.registry()?.get(source).and_then(Clone::clone);
        Ok(file.is_some_and(|file| file.changed()))
    }

    /// Inspect into a fresh descriptor; commit only if the old source remains registered.
    fn reload(&self, source: usize, cancel: Cancellation) -> Result<Source, QueryError> {
        let file = self
            .registry()?
            .get(source)
            .and_then(Clone::clone)
            .ok_or_else(|| QueryError::Invalid("File is no longer available".into()))?;
        self.inspect(&file.path, cancel, Some(source))
    }

    /// Remove registry metadata without taking the engine gate or touching the file.
    /// Existing workers own cloned source descriptors; their cancellation is independent.
    /// Slots are not reused, so late work cannot resolve an old ID to a newly opened file.
    fn close(&self, source: usize) -> Result<(), QueryError> {
        if let Some(slot) = self.registry()?.get_mut(source) {
            *slot = None;
        }
        Ok(())
    }

    /// Inspect one bounded footer relation under the same engine gate as data queries.
    /// Uses its own cancellation token and connection, so detail failures do not replace
    /// user data. Fingerprint checks reject details from an externally changed source.
    fn parquet_metadata(
        &self,
        source: usize,
        request: crate::ParquetRequest,
        cancel: Cancellation,
    ) -> Result<crate::ParquetPage, QueryError> {
        cancel.check()?;
        let file = self
            .registry()?
            .get(source)
            .and_then(Clone::clone)
            .ok_or_else(|| QueryError::Invalid("File is no longer available".into()))?;
        if file.info.parquet.is_none() {
            return Err(QueryError::Invalid(
                "File details require a Parquet source".into(),
            ));
        }
        let _guard = lock_engine(&self.gate, &cancel)?;
        file.ensure_current()?;
        let temp = crate::temporary::Directory::create()?;
        let conn = connection(temp.path())?;
        let _interrupt = stream::Interrupt::new(&conn, cancel.clone())?;
        register(&conn, &file.path, file.info.format)?;
        let result = parquet::page(&conn, request, &cancel);
        cancel.check()?;
        file.ensure_current()?;
        result
    }

    /// Resolve a registered source and start its background Arrow stream.
    ///
    /// Returns an error for cancellation, an unavailable source or stream startup
    /// failure. Startup waits for the result schema, so this method must run off the
    /// GUI thread even though execution owns a separate worker.
    fn query(
        &self,
        query: Query,
        cancel: Cancellation,
    ) -> Result<Box<dyn BatchCursor>, QueryError> {
        cancel.check()?;
        let file = self
            .registry()?
            .get(query.source)
            .and_then(Clone::clone)
            .ok_or_else(|| QueryError::Invalid("File is no longer available".into()))?;
        stream::start(file, query, cancel, self.gate.clone())
    }
}

#[cfg(test)]
mod admission_tests {
    use super::acquire_engine;
    use crate::{Cancellation, QueryError};
    use std::{
        future::Future,
        pin::pin,
        task::{Context, Poll, Waker},
    };
    use tokio::sync::Semaphore;

    /// Queue order is FIFO, a cancelled head releases its place, and permits return on drop.
    #[test]
    fn queued_cancellation_preserves_fifo_and_permits() -> Result<(), QueryError> {
        let gate = Semaphore::new(1);
        let owner_cancel = Cancellation::default();
        let owner = futures_lite::future::block_on(acquire_engine(&gate, &owner_cancel))?;
        let cancelled = Cancellation::default();
        let later = Cancellation::default();
        let mut first = pin!(acquire_engine(&gate, &cancelled));
        let mut second = pin!(acquire_engine(&gate, &later));
        let mut cx = Context::from_waker(Waker::noop());
        assert!(first.as_mut().poll(&mut cx).is_pending());
        assert!(second.as_mut().poll(&mut cx).is_pending());
        cancelled.cancel();
        assert!(matches!(
            first.as_mut().poll(&mut cx),
            Poll::Ready(Err(QueryError::Cancelled))
        ));
        assert!(second.as_mut().poll(&mut cx).is_pending());
        drop(owner);
        let next = futures_lite::future::block_on(second)?;
        assert_eq!(gate.available_permits(), 0);
        drop(next);
        assert_eq!(gate.available_permits(), 1);
        assert!(matches!(
            futures_lite::future::block_on(acquire_engine(&gate, &cancelled)),
            Err(QueryError::Cancelled)
        ));
        assert_eq!(gate.available_permits(), 1);
        Ok(())
    }

    /// An available permit is reserved for the oldest live waiter even if a newer one polls first.
    #[test]
    fn admission_does_not_overtake_older_waiter() -> Result<(), QueryError> {
        let gate = Semaphore::new(1);
        let cancel = Cancellation::default();
        let owner = futures_lite::future::block_on(acquire_engine(&gate, &cancel))?;
        let mut first = pin!(acquire_engine(&gate, &cancel));
        let mut second = pin!(acquire_engine(&gate, &cancel));
        let mut cx = Context::from_waker(Waker::noop());
        assert!(first.as_mut().poll(&mut cx).is_pending());
        assert!(second.as_mut().poll(&mut cx).is_pending());
        drop(owner);
        assert!(second.as_mut().poll(&mut cx).is_pending());
        let first = futures_lite::future::block_on(first)?;
        assert!(second.as_mut().poll(&mut cx).is_pending());
        drop(first);
        drop(futures_lite::future::block_on(second)?);
        assert_eq!(gate.available_permits(), 1);
        Ok(())
    }
}
