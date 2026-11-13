//! Local-file registry and resource-limited DuckDB connections.

use crate::stream;
use arrow_schema::SchemaRef;
use duckdb::{Config, Connection};
use rowglide_app::{Backend, BatchCursor, Cancellation, Query, QueryError, Source};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex, TryLockError},
    thread,
    time::Duration,
};

/// Registered local file metadata plus the schema obtained during inspection.
///
/// The canonical path identifies duplicate registrations. The schema is a snapshot;
/// queries reopen the file, so later external modifications are not isolated.
#[derive(Clone)]
pub(crate) struct FileSource {
    /// Stable registry handle and display metadata returned to callers.
    pub(crate) info: Source,
    /// Canonical local path reused for each independent query scan.
    pub(crate) path: PathBuf,
    /// Source schema captured when the file was first registered.
    pub(crate) schema: SchemaRef,
}

/// Local-file registry and serialized DuckDB query backend.
///
/// Registration and execution share one engine gate, limiting concurrent engine
/// budgets. Every query opens its own in-memory connection and scans the source
/// file directly; the registry holds neither result buffers nor live GUI objects.
#[derive(Default)]
pub struct DuckDbBackend {
    /// Registered sources; vector indices are their backend-local IDs.
    files: Mutex<Vec<FileSource>>,
    /// Serializes inspection and execution so requests cannot multiply engine budgets.
    gate: Arc<Mutex<()>>,
}

/// Convert an engine, I/O or thread error into a displayable backend failure.
///
/// This preserves its message as text; it does not retain the original error type
/// or an error-source chain.
pub(crate) fn error(error: impl std::fmt::Display) -> QueryError {
    QueryError::Backend(error.to_string())
}

/// Open an in-memory DuckDB connection with bounded execution resources.
///
/// Uses up to four threads, 512 MB engine memory and a 2 GB spill budget under
/// `temp`. Preserves source insertion order and disables automatic extension
/// loading/installing. The caller owns and cleans up the temporary directory.
/// Configuration or connection failures are returned as backend errors.
pub(crate) fn connection(temp: &Path) -> Result<Connection, QueryError> {
    let threads = thread::available_parallelism().map_or(2, |n| n.get().min(4));
    let config = Config::default()
        .max_memory("512MB")
        .map_err(error)?
        .threads(threads as i64)
        .map_err(error)?
        .enable_autoload_extension(false)
        .map_err(error)?
        .with("autoinstall_known_extensions", "false")
        .map_err(error)?
        .with("temp_directory", temp.to_string_lossy())
        .map_err(error)?
        .with("max_temp_directory_size", "2GB")
        .map_err(error)?
        .with("preserve_insertion_order", "true")
        .map_err(error)?;
    Connection::open_in_memory_with_flags(config).map_err(error)
}

/// Expose a local file as the connection-local `current_file` view.
///
/// Binds the UTF-8 path through a DuckDB variable rather than SQL interpolation.
/// `PARQUET` selects the Parquet reader; registry-validated CSV sources use bounded
/// automatic inference. Returns errors for non-UTF-8 paths or reader/view setup.
pub(crate) fn register(conn: &Connection, path: &Path, format: &str) -> Result<(), QueryError> {
    let path = path
        .to_str()
        .ok_or_else(|| QueryError::Invalid("File path must be valid UTF-8".into()))?;
    conn.execute("SET VARIABLE source_path = ?", [path])
        .map_err(error)?;
    let sql = if format == "PARQUET" {
        "CREATE VIEW current_file AS SELECT * FROM read_parquet(getvariable('source_path'))"
    } else {
        "CREATE VIEW current_file AS SELECT * FROM read_csv(getvariable('source_path'), sample_size=20480)"
    };
    conn.execute_batch(sql).map_err(error)
}

/// Wait for exclusive engine access while checking cancellation between attempts.
///
/// Uses short sleeps rather than an uninterruptible mutex lock. Returns a guard
/// that keeps the engine reserved, or an error for cancellation or a poisoned gate.
/// Call only from background work.
pub(crate) fn lock_engine<'a>(
    gate: &'a Mutex<()>,
    cancel: &Cancellation,
) -> Result<std::sync::MutexGuard<'a, ()>, QueryError> {
    loop {
        cancel.check()?;
        match gate.try_lock() {
            Ok(guard) => return Ok(guard),
            Err(TryLockError::Poisoned(_)) => return Err(error("Query worker failed")),
            Err(TryLockError::WouldBlock) => thread::sleep(Duration::from_millis(10)),
        }
    }
}

impl Backend for DuckDbBackend {
    /// Clone the registered-source metadata under the registry lock.
    ///
    /// Returns an empty snapshot if the registry mutex is poisoned; no file inspection
    /// or query execution is performed here.
    fn sources(&self) -> Vec<Source> {
        self.files.lock().map_or_else(
            |_| vec![],
            |files| files.iter().map(|file| file.info.clone()).collect(),
        )
    }

    /// Canonicalize, validate and inspect a CSV/Parquet path before registering it.
    ///
    /// Duplicate canonical paths reuse their existing ID. New files are inspected under
    /// the engine gate using a zero-row query to obtain schema without counting rows.
    /// The registry is checked again before insertion to handle concurrent opens.
    /// Errors include unsupported formats, missing files, cancellation and DuckDB
    /// inspection failures. The returned source has an unknown row count.
    fn open(&self, path: &Path, cancel: Cancellation) -> Result<Source, QueryError> {
        cancel.check()?;
        let path = path.canonicalize().map_err(error)?;
        if !path.is_file() {
            return Err(error("Select a regular CSV or Parquet file"));
        }
        let format = match path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("parquet" | "pq") => "PARQUET",
            Some("csv") => "CSV",
            _ => return Err(error("Supported file types: .parquet, .pq, .csv")),
        };
        if let Some(file) = self
            .files
            .lock()
            .map_err(error)?
            .iter()
            .find(|file| file.path == path)
        {
            return Ok(file.info.clone());
        }
        let _guard = lock_engine(&self.gate, &cancel)?;
        let temp = tempfile::tempdir().map_err(error)?;
        let conn = connection(temp.path())?;
        let _interrupt = stream::Interrupt::new(&conn, cancel.clone())?;
        register(&conn, &path, format)?;
        let mut statement = conn
            .prepare("SELECT * FROM current_file LIMIT 0")
            .map_err(error)?;
        let schema = statement.stream_arrow([]).map_err(error)?.get_schema();
        cancel.check()?;
        let mut files = self.files.lock().map_err(error)?;
        if let Some(file) = files.iter().find(|file| file.path == path) {
            return Ok(file.info.clone());
        }
        let info = Source {
            id: files.len(),
            name: path.file_name().map_or_else(
                || path.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            ),
            format,
            rows: None,
            description: "Local file",
        };
        files.push(FileSource {
            info: info.clone(),
            path,
            schema,
        });
        Ok(info)
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
            .files
            .lock()
            .map_err(error)?
            .get(query.source)
            .cloned()
            .ok_or_else(|| error("File is no longer available"))?;
        stream::start(file, query, cancel, self.gate.clone())
    }
}
