//! Database layer: SQLite connection pool, schema migration, CRUD queries, and read wrappers.

pub mod document;
pub mod migrate;
pub mod ops;
pub mod pool;
pub mod query;

pub use pool::DbPool;
