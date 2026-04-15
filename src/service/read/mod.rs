//! Service-layer read operations for collections and globals.
//!
//! Centralizes the read lifecycle (hooks -> query -> hydrate -> populate -> strip)
//! shared across admin, gRPC, MCP, and Lua CRUD surfaces.

mod count;
mod find;
mod find_by_id;
mod get_global;
mod options;
pub(crate) mod post_process;
mod search;
mod validate_filters;

pub use count::count_documents;
pub use find::find_documents;
pub use find_by_id::find_document_by_id;
pub use get_global::get_global_document;
pub use options::{ReadOptions, ReadOptionsBuilder};
pub use search::search_documents;
pub use validate_filters::{validate_access_constraints, validate_user_filters};
