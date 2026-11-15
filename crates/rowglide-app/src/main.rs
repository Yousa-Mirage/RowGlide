//! Application entry point and concrete backend composition.
//!
//! The executable chooses the query engine and delegates native presentation to
//! `rowglide-ui`. Shared models and backend behavior live in `rowglide-core`.

mod arguments;
mod config;

use rowglide_core::{DuckDbBackend, QueryError, ResultStore, cleanup_temporary_storage};
use std::sync::Arc;

/// Construct the local-file DuckDB backend and start the native UI on the main thread.
///
/// Backend selection happens here so the UI can also be launched with another
/// implementation of the same contract, such as the deterministic mock backend.
/// Startup cache recovery runs in the background and preserves other live instances.
/// Parse startup paths first so help, version and argument errors do not initialize
/// the backend or GUI. File validation and loading belong to the UI's background opener.
fn main() -> Result<(), QueryError> {
    let files = match arguments::files(std::env::args_os()) {
        Ok(files) => files,
        Err(exit) if exit.status.is_ok() => {
            println!("{}", exit.output);
            return Ok(());
        }
        Err(exit) => {
            eprintln!("{}", exit.output);
            std::process::exit(2);
        }
    };
    let results = ResultStore::new(config::cache_limits()?)?;
    if let Err(error) = std::thread::Builder::new()
        .name("cache-recovery".into())
        .spawn(|| {
            if let Err(error) = cleanup_temporary_storage() {
                eprintln!("Cannot reclaim temporary cache: {error}");
            }
        })
    {
        eprintln!("Cannot start temporary cache recovery: {error}");
    }
    rowglide_ui::run(Arc::new(DuckDbBackend::default()), results, files);
    Ok(())
}
