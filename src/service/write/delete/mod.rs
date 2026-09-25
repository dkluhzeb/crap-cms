//! Core delete operation for collections.

mod document;
mod execute;
mod purge;

#[cfg(all(test, feature = "sqlite"))]
mod test_support;

pub(crate) use document::delete_document_in_conn;
pub(crate) use purge::{cancel_image_jobs, purge_document};
