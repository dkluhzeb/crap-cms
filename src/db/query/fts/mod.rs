//! Full-text search helpers: index management, search, sync on writes.
//!
//! Supports `SQLite` (FTS5) and `PostgreSQL` (tsvector + GIN).

mod extract;
mod fields;
mod index;
mod layout;
mod search;
mod sync;

pub use fields::{get_fts_columns, get_fts_fields, validate_searchable_fields};
pub use index::{FtsIndex, FtsIndexBuilder};
pub(crate) use search::{FtsSearch, fts_rank_order_by, fts_where_clause};
pub use sync::{FtsShape, fts_delete, fts_shape, fts_table_exists, fts_upsert, sync_fts_table};
