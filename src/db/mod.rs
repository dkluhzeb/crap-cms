//! Database layer: SQLite connection pool, schema migration, CRUD queries, and read wrappers.

pub mod pool;
pub mod migrate;
pub mod query;
pub mod document;
pub mod ops;

pub use pool::DbPool;
