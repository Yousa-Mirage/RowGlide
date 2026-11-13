//! DuckDB implementation: file registry, SQL construction and bounded Arrow transport.
//!
//! Engine details stay private to this module. Callers construct `DuckDbBackend`
//! and execute operations through the shared `Backend` contract.

mod engine;
mod sql;
mod stream;

pub use engine::DuckDbBackend;
