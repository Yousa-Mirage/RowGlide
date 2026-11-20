//! Real-file freshness checks and explicit reload behavior.

use duckdb::Connection;
use rowglide_core::{
    Backend, Cancellation, DuckDbBackend, ParquetRequest, Preview, Query, QueryError,
};
use std::{error::Error, fs, path::Path};

/// Write a small Parquet fixture via a bound destination path.
fn parquet(path: &Path, rows: usize) -> Result<(), Box<dyn Error>> {
    let connection = Connection::open_in_memory()?;
    connection.execute(
        "SET VARIABLE destination = ?",
        [path.to_str().ok_or("Non-UTF-8 fixture")?],
    )?;
    connection.execute_batch(&format!("COPY (SELECT range AS id FROM range({rows})) TO (getvariable('destination')) (FORMAT PARQUET)"))?;
    Ok(())
}

/// External replacement rejects both data and footer reads; successful reload updates the source.
#[test]
fn changed_parquet_requires_reload() -> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("data.parquet");
    parquet(&path, 2)?;
    let backend = DuckDbBackend::default();
    let source = backend.open(&path, Cancellation::default())?;
    assert!(!backend.source_changed(source.id)?);
    parquet(&path, 23)?;
    assert!(backend.source_changed(source.id)?);
    assert!(matches!(
        backend.query(
            Query {
                source: source.id,
                ..Query::default()
            },
            Cancellation::default()
        ),
        Err(QueryError::SourceChanged)
    ));
    assert!(matches!(
        backend.parquet_metadata(source.id, ParquetRequest::new(0)?, Cancellation::default()),
        Err(QueryError::SourceChanged)
    ));
    let reloaded = backend.reload(source.id, Cancellation::default())?;
    assert_eq!(reloaded.id, source.id);
    assert_eq!(reloaded.rows, Some(23));
    assert!(!backend.source_changed(source.id)?);
    assert!(matches!(
        backend.query(
            Query {
                source: source.id,
                ..Query::default()
            },
            Cancellation::default()
        ),
        Err(QueryError::Filter(
            rowglide_core::FilterError::SchemaChanged
        ))
    ));
    let cancel = Cancellation::default();
    let preview = Preview::collect(
        backend.query(
            Query {
                source: source.id,
                schema_revision: reloaded.schema_revision,
                ..Query::default()
            },
            cancel.clone(),
        )?,
        &cancel,
    )?;
    assert_eq!(preview.rows, 23);
    Ok(())
}

/// Missing/corrupt files preserve registration on failed reload; restoration needs explicit reload.
#[test]
fn failures_preserve_the_old_registration() -> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("data.parquet");
    parquet(&path, 2)?;
    let backend = DuckDbBackend::default();
    let source = backend.open(&path, Cancellation::default())?;
    fs::remove_file(&path)?;
    assert!(backend.source_changed(source.id)?);
    assert!(backend.reload(source.id, Cancellation::default()).is_err());
    fs::write(&path, "invalid parquet")?;
    assert!(backend.reload(source.id, Cancellation::default()).is_err());
    assert_eq!(backend.sources()[0].rows, Some(2));
    parquet(&path, 4)?;
    assert!(backend.source_changed(source.id)?);
    let cancel = Cancellation::default();
    cancel.cancel();
    assert!(matches!(
        backend.reload(source.id, cancel),
        Err(QueryError::Cancelled)
    ));
    assert!(backend.source_changed(source.id)?);
    assert_eq!(
        backend.reload(source.id, Cancellation::default())?.rows,
        Some(4)
    );
    backend.close(source.id)?;
    assert!(!backend.source_changed(source.id)?);
    assert!(backend.reload(source.id, Cancellation::default()).is_err());
    Ok(())
}

/// Modification time detects equal-length edits; reverting metadata cannot clear a latched warning.
#[test]
fn equal_length_changes_are_latched() -> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("data.csv");
    fs::write(&path, "id\n1\n")?;
    let backend = DuckDbBackend::default();
    let source = backend.open(&path, Cancellation::default())?;
    let modified = fs::metadata(&path)?.modified()?;
    fs::write(&path, "id\n2\n")?;
    let file = fs::File::options().write(true).open(&path)?;
    file.set_modified(modified + std::time::Duration::from_secs(10))?;
    assert!(backend.source_changed(source.id)?);
    file.set_modified(modified)?;
    assert!(backend.source_changed(source.id)?);
    backend.reload(source.id, Cancellation::default())?;
    assert!(!backend.source_changed(source.id)?);
    Ok(())
}
