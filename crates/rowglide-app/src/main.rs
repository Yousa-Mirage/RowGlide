//! Application entry point and concrete backend composition.
//!
//! The executable chooses the query engine and delegates native presentation to
//! `rowglide-ui`. Shared models and backend behavior live in `rowglide-core`.

use rowglide_core::DuckDbBackend;
use std::sync::Arc;

/// Construct the local-file DuckDB backend and start the native UI on the main thread.
///
/// Backend selection happens here so the UI can also be launched with another
/// implementation of the same contract, such as the deterministic mock backend.
fn main() {
    rowglide_ui::run(Arc::new(DuckDbBackend::default()));
}
