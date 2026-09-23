//! Shared collection handler utilities — form errors, update, delete, list helpers.

mod auth_fields;
mod delete;
mod form_errors;
mod image;
mod update;

// Re-export list helpers
pub(super) use super::list_helpers::{
    FilterPillInputs, ListFieldAccess, active_filter_count, build_column_options,
    build_filter_fields, build_filter_pills, compute_cells, resolve_columns, title_label,
};

// Re-export form error rendering
pub(super) use form_errors::{SubmittedMeta, WriteErrorParams, handle_collection_write_error};

// Re-export the synthesized auth-collection inputs
pub(super) use auth_fields::{locked_field, password_field};

// Re-export shared helpers
pub(super) use image::thumbnail_url;

// Re-export update/delete handlers
pub(super) use delete::delete_action_impl;
pub(super) use update::{UpdateRequest, do_update};
