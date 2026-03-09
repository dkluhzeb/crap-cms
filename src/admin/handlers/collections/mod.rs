//! Collection CRUD handlers: list, create, edit, delete.

pub mod forms;
pub mod list;
pub mod create;
pub mod edit;
pub mod delete;
pub mod versions;
pub mod search;

// Re-export shared helpers so submodules can access them via `super::shared`.
// These are needed because `handlers::shared` is a private module of `handlers`;
// grandchild modules cannot use `super::super::shared` directly.
pub(super) use super::shared::{
    PaginationParams,
    get_user_doc, get_event_user, strip_denied_fields,
    check_access_or_forbid, extract_editor_locale, build_locale_template_data,
    is_non_default_locale, auto_label_from_name, url_decode,
    parse_where_params, validate_sort, build_list_url, is_column_eligible,
    extract_where_params,
    build_field_contexts, enrich_field_contexts,
    apply_display_conditions, split_sidebar_fields,
    translate_validation_errors,
    version_to_json, fetch_version_sidebar_data,
    forbidden, redirect_response, htmx_redirect, html_with_toast,
    render_or_error, not_found, server_error,
};

// Re-export all public handler items so external code doesn't need to change import paths.
pub use list::{list_collections, list_items, save_user_settings};
pub use create::{create_form, create_action};
pub use edit::{edit_form, update_action_post};
pub use delete::{delete_confirm, delete_action_simple};
pub use versions::{restore_version, list_versions_page, evaluate_conditions, EvaluateConditionsRequest};
pub use search::{search_collection, SearchQuery};
