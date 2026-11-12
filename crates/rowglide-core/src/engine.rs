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

#[derive(Clone)]
pub(crate) struct FileSource {
    pub(crate) info: Source,
    pub(crate) path: PathBuf,
    pub(crate) schema: SchemaRef,
}

/// Files remain on disk; each query scans only what DuckDB needs.
#[derive(Default)]
pub struct DuckDbBackend {
    files: Mutex<Vec<FileSource>>,
    // Serialize query engines so rapid GUI requests cannot multiply budgets.
    gate: Arc<Mutex<()>>,
}

pub(crate) fn error(error: impl std::fmt::Display) -> QueryError {
    QueryError::Backend(error.to_string())
}

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
        .map_err(error)?;
    Connection::open_in_memory_with_flags(config).map_err(error)
}

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
    fn sources(&self) -> Vec<Source> {
        self.files.lock().map_or_else(
            |_| vec![],
            |files| files.iter().map(|file| file.info.clone()).collect(),
        )
    }

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
