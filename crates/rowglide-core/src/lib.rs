//! Shared data contracts, pagination and query backends for Rowglide.
//!
//! This crate contains no GUI objects. UI code consumes the exported model and
//! `Backend` interface; the application entry point selects a concrete backend.
//! DuckDB-specific connection, SQL and worker details remain internal.

mod backend;
mod duckdb;
mod mock;
mod model;
mod pagination;

pub use backend::{Backend, BatchCursor, Cancellation, QueryError};
pub use duckdb::DuckDbBackend;
pub use mock::{EMPTY_SQL, MockBackend, PREVIEW_SQL};
pub use model::{Preview, Query, Sort, Source};
pub use pagination::{BrowsePage, PageResult};

mod session;
pub use session::{QuerySession, SessionEvent};

mod result;
pub use result::{QueryResult, ResultLimits, ResultPhase, ResultSnapshot};
