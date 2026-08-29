//! Core write operations for collections, accepting `&dyn WriteHooks` for hook abstraction.
//!
//! These functions operate on an existing connection/transaction. The caller is responsible
//! for transaction management (open, commit/rollback). This allows both pool-based callers
//! (admin, gRPC, MCP) and in-transaction callers (Lua CRUD) to share the same code.

mod create;
mod delete;
mod update;
mod update_many_single;
mod validate;

use crate::service::ServiceError;

pub(crate) use create::check_create_access;
pub use create::create_document_in_conn;
pub(crate) use delete::delete_document_in_conn;
pub(crate) use update::check_update_access;
pub(crate) use update::update_document_in_conn;
pub(crate) use update_many_single::update_many_single_in_conn;
pub use validate::{ValidateContext, validate_document, validate_outcome};
