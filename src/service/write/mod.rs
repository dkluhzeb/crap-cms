//! Core write operations for collections, accepting `&dyn WriteHooks` for hook abstraction.
//!
//! These functions operate on an existing connection/transaction. The caller is responsible
//! for transaction management (open, commit/rollback). This allows both pool-based callers
//! (admin, gRPC, MCP) and in-transaction callers (Lua CRUD) to share the same code.

mod admission;
mod admit;
mod create;
mod delete;
mod delete_event;
mod event_row;
mod pending_draft;
mod update;
mod update_many_single;
mod upload_files;
mod upload_restore;
mod validate;

use crate::service::ServiceError;

pub(crate) use admission::{
    PendingDraft, admit_create_input, admit_global_update_input, admit_update_input,
};
pub(crate) use create::check_create_access;
pub(crate) use create::create_document_gated;
pub use create::create_document_in_conn;
pub(crate) use delete::{cancel_image_jobs, delete_document_in_conn, purge_document};
pub use delete_event::PurgeEvents;
pub(crate) use delete_event::{DeleteEvent, read_delete_event};
pub(crate) use pending_draft::{adopt_pending_draft, adopt_pending_global_draft};
pub(crate) use update::update_document_gated;
#[cfg(test)]
pub(crate) use update::update_document_in_conn;
pub(crate) use update::{
    check_update_access, reject_locale_locked_fields, stored_fields_for_update_rules,
};
pub(crate) use update_many_single::update_many_single_in_conn;
pub(in crate::service::write) use upload_files::stored_row;
pub(crate) use upload_files::{
    UploadSettle, document_file_keys, owned_file_keys, settle_upload_write, warn_orphaned_files,
};
pub(crate) use upload_restore::{adopt_held_variants, restored_file_conversions};
pub use validate::{ValidateContext, validate_document, validate_outcome};
