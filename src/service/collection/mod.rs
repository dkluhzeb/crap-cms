//! Collection CRUD orchestration: create, update, unpublish, delete, undelete.
//!
//! Each function wraps before-hooks -> DB operation -> after-hooks in a single transaction.

mod create;
mod delete;
mod undelete;
mod unpublish;
mod update;

pub use create::{create_document, create_document_with_conn};
pub use delete::{delete_document, delete_document_with_conn};
pub use undelete::{undelete_document, undelete_document_core};
pub use unpublish::{unpublish_document, unpublish_document_core};
pub use update::{update_document, update_document_with_conn};
