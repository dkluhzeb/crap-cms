//! Shared helper functions for admin handlers (collections + globals).

mod access;
mod document;
mod locale;
mod pagination;
pub(crate) mod paths;
pub(crate) mod response;
mod versions;

// Re-export field context functions from the dedicated module.
pub(super) use crate::admin::handlers::field_context::{
    EnrichOptions, apply_display_conditions, build_field_contexts, enrich_field_contexts,
    split_sidebar_fields,
};

// Re-export query utilities from the dedicated module.
pub(crate) use super::query::{
    ListUrlContext, extract_where_params, is_column_eligible, parse_where_params, url_decode,
    validate_sort,
};

// access
pub(crate) use access::{
    EvaluateConditionsRequest, check_access_or_forbid, compute_denied_read_fields,
    evaluate_condition_results, get_user_doc, has_read_access,
};

// document
pub(crate) use document::{
    auto_label_from_name, compute_row_label, flatten_document_values, lookup_ref_count,
    translate_validation_errors,
};

// locale
pub(crate) use locale::{build_locale_template_data, extract_editor_locale, is_non_default_locale};

// pagination
pub use pagination::{Pagination, PaginationParams};

// response
pub(crate) use response::{
    forbidden, html_with_toast, htmx_redirect, htmx_redirect_with_created, not_found,
    redirect_response, render_or_error, server_error, toast_only_error,
};

// versions
pub(crate) use versions::{
    extract_doc_status, fetch_version_sidebar_data, load_version_with_missing_relations,
    version_to_json,
};
