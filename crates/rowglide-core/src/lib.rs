//! Query engines behind the engine-independent application interface.

mod engine;
mod mock;
mod sql;
mod stream;

pub use engine::DuckDbBackend;
pub use mock::{EMPTY_SQL, MockBackend, PREVIEW_SQL};
