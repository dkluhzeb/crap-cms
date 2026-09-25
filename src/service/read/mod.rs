//! Service-layer read operations for collections and globals.
//!
//! Centralizes the read lifecycle (hooks -> query -> hydrate -> populate -> strip)
//! shared across admin, gRPC, MCP, and Lua CRUD surfaces.

mod count;
mod find;
mod find_by_id;
mod get_global;
mod populated_strip;
pub(crate) mod post_process;
mod query_access;
mod query_probe;
mod search;
mod validate_filters;

pub use count::{CollectionStats, collection_stats, count_documents};
pub use find::find_documents;
pub use find_by_id::{find_document_by_id, find_draft_view_stored, read_own_document};
pub use get_global::get_global_document;
pub(crate) use get_global::unpublished_global;
pub(crate) use populated_strip::join_child_readable;
pub(crate) use query_access::reject_unreadable_filter_fields;
pub use query_access::{
    QueryFieldRefs, is_hidden_query_path, query_field_paths, unreadable_query_paths,
};
pub use search::search_documents;
pub use validate_filters::{
    is_system_filter_path, validate_access_constraint_locales, validate_access_constraints,
    validate_user_filters,
};
