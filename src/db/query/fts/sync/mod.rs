//! FTS index synchronization, upsert, and delete operations.
//!
//! Supports `SQLite` (FTS5 virtual tables) and `PostgreSQL` (tsvector + GIN index).

mod delete;
mod helpers;
mod migration;
mod shape;
mod upsert;

#[cfg(test)]
mod test_helpers;

pub use delete::fts_delete;
pub use migration::sync_fts_table;
pub use shape::{FtsShape, fts_shape, fts_table_exists};
pub use upsert::fts_upsert;
