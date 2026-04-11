//! Core write operations for collections, accepting `&dyn WriteHooks` for hook abstraction.
//!
//! These functions operate on an existing connection/transaction. The caller is responsible
//! for transaction management (open, commit/rollback). This allows both pool-based callers
//! (admin, gRPC, MCP) and in-transaction callers (Lua CRUD) to share the same code.

mod create;
mod delete;
pub(crate) mod helpers;
mod update;
mod update_many_single;
mod validate;

use crate::service::ServiceError;

pub use create::create_document_core;
pub use delete::{DeleteResult, delete_document_core};
pub use update::update_document_core;
pub use update_many_single::update_many_single_core;
pub use validate::{ValidateContext, validate_document};
