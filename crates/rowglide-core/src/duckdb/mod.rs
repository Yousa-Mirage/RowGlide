//! DuckDB implementation: file registry, SQL construction and bounded Arrow transport.
//!
//! Engine details stay private to this module. Callers construct `DuckDbBackend`
//! and execute operations through the shared `Backend` contract.

mod engine;
mod filter;
mod freshness;
mod parquet;
mod sql;
mod stream;

pub use engine::DuckDbBackend;

/// Expose the real compiler to crate-level predicate equivalence tests only.
#[cfg(test)]
pub(crate) fn test_filter_sql(
    query: &crate::Query,
    schema: &arrow_schema::Schema,
) -> Result<(String, Vec<duckdb::types::Value>), crate::QueryError> {
    sql::build(query, schema)
}
