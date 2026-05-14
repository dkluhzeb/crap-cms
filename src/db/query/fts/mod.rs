//! Full-text search helpers: index management, search, sync on writes.
//!
//! Supports `SQLite` (FTS5) and `PostgreSQL` (tsvector + GIN).

mod extract;
mod fields;
mod search;
mod sync;

pub use fields::{get_fts_columns, get_fts_fields};
pub use search::fts_search;
pub(crate) use search::fts_where_clause;
pub use sync::{fts_delete, fts_upsert, sync_fts_table};
