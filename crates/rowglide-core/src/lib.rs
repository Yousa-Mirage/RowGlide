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
mod parquet;
pub use parquet::{ParquetSummary, SchemaRow};
mod temporary;

pub use temporary::cleanup_temporary_storage;

pub use backend::{Backend, BatchCursor, Cancellation, QueryError};
pub use duckdb::DuckDbBackend;
pub use mock::{EMPTY_SQL, MockBackend, PREVIEW_SQL};
pub use model::{Preview, Query, Sort, Source};
pub use pagination::{BrowsePage, PageResult};

mod session;
pub use session::{QuerySession, SessionEvent};

mod result;
pub use result::{
    CacheLimits, CacheUsage, QueryResult, ResultLimits, ResultPhase, ResultSnapshot, ResultStore,
    ResultUpdates,
};

mod clipboard;
pub use clipboard::{CopyError, CopyFormat, CopyLimits, serialize_cell};

mod recent;
pub use recent::{RecentError, RecentFiles};

mod filter;
pub use filter::{
    CompareOp, FilterCondition, FilterError, FilterKind, FilterValue, filter_data_type,
    filter_kind, filter_operators, parse_filter_value, validate_filters,
};
