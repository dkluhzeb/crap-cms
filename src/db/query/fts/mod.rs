//! FTS5 full-text search helpers: index management, search, sync on writes.

mod fields;
mod prosemirror;
mod search;
mod sync;

pub use fields::{get_fts_columns, get_fts_fields};
pub use prosemirror::{extract_prosemirror_text, extract_prosemirror_text_with_nodes};
pub use search::{fts_search, fts_where_clause, sanitize_fts_query};
pub use sync::{fts_delete, fts_upsert, fts_upsert_with_registry, sync_fts_table};
