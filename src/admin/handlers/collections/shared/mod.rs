//! Shared collection handler utilities — form errors, update, delete, list helpers.

mod delete;
mod form_errors;
mod image;
mod update;
mod upload;

// Re-export list helpers
pub(super) use super::list_helpers::{
    build_column_options, build_filter_fields, build_filter_pills, compute_cells, resolve_columns,
};

// Re-export form error rendering
pub(super) use form_errors::{
    WriteErrorParams, handle_collection_write_error, render_edit_upload_error, render_upload_error,
};

// Re-export shared helpers
pub(super) use image::thumbnail_url;

// Re-export upload processing
pub(super) use upload::{UploadParams, UploadResult, process_collection_upload};

// Re-export update/delete handlers
pub(super) use delete::delete_action_impl;
pub(super) use update::do_update;
