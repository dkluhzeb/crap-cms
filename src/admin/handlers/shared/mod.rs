//! Shared helper functions for admin handlers (collections + globals).

mod access;
mod breadcrumbs;
mod db_error;
mod document;
mod form_fields;
pub(crate) mod hx;
mod locale;
mod pagination;
pub(crate) mod paths;
pub(crate) mod response;
mod versions;

// database errors
pub(crate) use db_error::db_error_status;

// breadcrumb base-chains
pub(crate) use breadcrumbs::{collection_base, collection_item_base, global_base};

// Re-export field context functions from the dedicated module.
pub(super) use crate::admin::handlers::field_context::{
    EnrichOptions, apply_display_conditions, build_field_contexts, date_picker_values,
    enrich_field_contexts, split_sidebar_fields, tag_values_of,
};

// what the admin form renders
pub(crate) use form_fields::{admin_form_fields, for_each_admin_form_leaf, renders_in_admin_form};

// Re-export query utilities from the dedicated module.
pub(crate) use super::query::{
    ListUrlContext, extract_status_filter, extract_where_params, is_column_eligible,
    is_meta_column, is_sortable_column, parse_where_params, url_decode, validate_sort,
};

// access
pub(crate) use access::{
    EvaluateConditionsRequest, check_access_or_forbid, compute_denied_read_fields,
    evaluate_condition_results, get_user_doc, has_access_with_conn, has_page_access,
    has_page_access_with_conn, is_admin_visible, is_admin_visible_with_conn,
};

// document
pub(crate) use document::{
    auto_label_from_name, compute_row_label, flatten_document_values, lookup_ref_count,
    translate_validation_errors, value_to_form_string,
};

// locale
pub(crate) use locale::{
    editor_locale_ctx, editor_read_ctx, extract_editor_locale, is_non_default_locale,
    parse_request_locale, strip_locale_locked_form_fields,
};

// pagination
pub use hx::HxNav;
pub use pagination::{Pagination, PaginationParams};

// response
pub(crate) use response::{
    PageRequest, bad_request, forbidden, htmx_inline_created, htmx_redirect,
    htmx_redirect_with_created, json_bad_request, json_conflict, json_forbidden, json_not_found,
    json_server_error, not_found, page_with_toast, redirect_response, render_auth_page,
    render_page, require_collection, require_collection_json, require_global, server_error,
    service_error_to_admin_response, task_join_error_response, toast_only_error,
};

// versions
pub(crate) use versions::{
    extract_doc_status, fetch_version_sidebar_data, finish_version_restore,
    load_version_with_restore_gaps, version_to_json,
};
