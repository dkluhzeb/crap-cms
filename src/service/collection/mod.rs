//! Collection CRUD orchestration: create, update, unpublish, delete, undelete.
//!
//! Each function wraps before-hooks -> DB operation -> after-hooks in a single transaction.

mod create;
mod create_many;
mod delete;
mod delete_many;
mod undelete;
mod unpublish;
mod update;
mod update_many;

pub use create::create_document;
pub use create_many::{CreateManyItem, CreateManyOptions, CreateManyResult, create_many};
pub use delete::delete_document;
pub use delete_many::{DeleteManyOptions, DeleteManyResult, delete_many};
pub use undelete::{undelete_document, undelete_document_core};
pub use unpublish::{unpublish_document, unpublish_document_core};
pub use update::update_document;
pub use update_many::{UpdateManyOptions, UpdateManyResult, update_many};
