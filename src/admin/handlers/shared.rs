//! Shared helper functions for admin handlers (collections + globals).

use axum::{
    http::StatusCode,
    response::{Html, IntoResponse, Redirect},
    Extension,
};
use serde::Deserialize;
use std::collections::HashMap;

use crate::admin::AdminState;
use crate::admin::context::{ContextBuilder, PageType};
use crate::admin::translations::Translations;
use crate::core::auth::AuthUser;
use crate::core::collection::{CollectionDefinition, VersionsConfig};
use crate::core::document::VersionSnapshot;
use crate::core::field::{FieldDefinition, FieldType};
use crate::core::validate::ValidationError;
use crate::db::query::{self, AccessResult, Filter, FilterClause, FilterOp, LocaleContext};
use crate::db::DbPool;

// Re-export field context functions from the dedicated module.
pub(super) use super::field_context::{
    build_field_contexts, apply_display_conditions, split_sidebar_fields, enrich_field_contexts,
};

/// Query parameters for paginated collection list views.
#[derive(Debug, Deserialize)]
pub struct PaginationParams {
    pub page: Option<i64>,
    pub per_page: Option<i64>,
    pub search: Option<String>,
    pub sort: Option<String>,
}

/// Extract the editor locale from the `crap_editor_locale` cookie.
/// Falls back to the config's default locale if the cookie is absent or invalid.
/// Returns `None` if locales are not enabled.
pub(super) fn extract_editor_locale(headers: &axum::http::HeaderMap, config: &crate::config::LocaleConfig) -> Option<String> {
    if !config.is_enabled() {
        return None;
    }
    let cookie_str = headers.get(axum::http::header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let raw = crate::admin::server::extract_cookie(cookie_str, "crap_editor_locale");
    let locale = raw.unwrap_or(&config.default_locale);
    // Validate against configured locales
    if config.locales.contains(&locale.to_string()) {
        Some(locale.to_string())
    } else {
        Some(config.default_locale.clone())
    }
}

/// Extract the user document from AuthUser extension (for access checks).
pub(super) fn get_user_doc(auth_user: &Option<Extension<AuthUser>>) -> Option<&crate::core::Document> {
    auth_user.as_ref().map(|Extension(au)| &au.user_doc)
}


/// Extract an EventUser from the AuthUser extension (for SSE event attribution).
pub(super) fn get_event_user(auth_user: &Option<Extension<AuthUser>>) -> Option<crate::core::event::EventUser> {
    auth_user.as_ref().map(|Extension(au)| crate::core::event::EventUser::new(au.claims.sub.clone(), au.claims.email.clone()))
}

/// Strip denied fields from a document's fields map.
pub(super) fn strip_denied_fields(
    fields: &mut HashMap<String, serde_json::Value>,
    denied: &[String],
) {
    for name in denied {
        fields.remove(name);
    }
}

/// Helper to check collection/global-level access. Returns AccessResult or renders a 403 page.
#[allow(clippy::result_large_err)]
pub(super) fn check_access_or_forbid(
    state: &AdminState,
    access_ref: Option<&str>,
    auth_user: &Option<Extension<AuthUser>>,
    id: Option<&str>,
    data: Option<&HashMap<String, serde_json::Value>>,
) -> Result<AccessResult, axum::response::Response> {
    // No access function configured = always allowed (skip pool.get + VM acquire)
    if access_ref.is_none() {
        return Ok(AccessResult::Allowed);
    }
    let user_doc = get_user_doc(auth_user);
    let mut conn = state.pool.get()
        .map_err(|_| forbidden(state, "Database error").into_response())?;
    let tx = conn.transaction()
        .map_err(|_| forbidden(state, "Database error").into_response())?;
    let result = state.hook_runner.check_access(access_ref, user_doc, id, data, &tx)
        .map_err(|e| {
            tracing::error!("Access check error: {}", e);
            forbidden(state, "Access check failed").into_response()
        })?;
    tx.commit()
        .map_err(|_| forbidden(state, "Database error").into_response())?;
    Ok(result)
}

/// Build locale template context (selector data) from config + current locale.
/// Returns `(locale_ctx_for_db, template_json)` where template_json has
/// `has_locales`, `current_locale`, `locales` (array with value/label/selected).
pub(super) fn build_locale_template_data(
    state: &AdminState,
    requested_locale: Option<&str>,
) -> (Option<LocaleContext>, serde_json::Value) {
    let config = &state.config.locale;
    if !config.is_enabled() {
        return (None, serde_json::json!({}));
    }
    let current = requested_locale.unwrap_or(&config.default_locale);
    let locale_ctx = LocaleContext::from_locale_string(Some(current), config);
    let locales: Vec<serde_json::Value> = config.locales.iter().map(|l| {
        serde_json::json!({
            "value": l,
            "label": l.to_uppercase(),
            "selected": l == current,
        })
    }).collect();
    let data = serde_json::json!({
        "has_locales": true,
        "current_locale": current,
        "locales": locales,
    });
    (locale_ctx, data)
}

/// Auto-generate a label from a field name (e.g. "my_field" -> "My Field").
pub(super) fn auto_label_from_name(name: &str) -> String {
    crate::core::field::to_title_case(name)
}

/// Parse `where[field][op]=value` parameters from a raw query string.
/// Returns empty vec for malformed/invalid params. Best-effort parsing.
pub(super) fn parse_where_params(
    raw_query: &str,
    def: &CollectionDefinition,
) -> Vec<FilterClause> {
    let mut filters = Vec::new();
    let system_cols = ["id", "created_at", "updated_at", "_status"];

    for part in raw_query.split('&') {
        let Some((key, value)) = part.split_once('=') else { continue };
        let value = url_decode(value);

        // Match where[field][op]
        let key = url_decode(key);
        let Some(rest) = key.strip_prefix("where[") else { continue };
        let Some((field, rest)) = rest.split_once("][") else { continue };
        let Some(op_str) = rest.strip_suffix(']') else { continue };

        // Validate field exists
        let field_valid = system_cols.contains(&field)
            || def.fields.iter().any(|f| f.name == field);
        if !field_valid {
            continue;
        }

        let op = match op_str {
            "equals" => FilterOp::Equals(value),
            "not_equals" => FilterOp::NotEquals(value),
            "contains" => FilterOp::Contains(value),
            "like" => FilterOp::Like(value),
            "gt" => FilterOp::GreaterThan(value),
            "lt" => FilterOp::LessThan(value),
            "gte" => FilterOp::GreaterThanOrEqual(value),
            "lte" => FilterOp::LessThanOrEqual(value),
            "exists" => FilterOp::Exists,
            "not_exists" => FilterOp::NotExists,
            _ => continue,
        };

        filters.push(FilterClause::Single(Filter {
            field: field.to_string(),
            op,
        }));
    }

    filters
}

/// Simple percent-decoding for URL query values.
pub(super) fn url_decode(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.bytes();
    while let Some(b) = chars.next() {
        if b == b'+' {
            result.push(' ');
        } else if b == b'%' {
            let hi = chars.next().and_then(|c| (c as char).to_digit(16));
            let lo = chars.next().and_then(|c| (c as char).to_digit(16));
            if let (Some(h), Some(l)) = (hi, lo) {
                result.push((h * 16 + l) as u8 as char);
            }
        } else {
            result.push(b as char);
        }
    }
    result
}

/// Validate a sort field name against the collection definition.
/// Strips leading `-` (descending) before validation.
/// Returns the validated sort string (with `-` prefix if present), or None.
pub(super) fn validate_sort(sort: &str, def: &CollectionDefinition) -> Option<String> {
    let field_name = sort.strip_prefix('-').unwrap_or(sort);
    let system_cols = ["id", "created_at", "updated_at", "_status"];
    let valid = system_cols.contains(&field_name)
        || def.fields.iter().any(|f| f.name == field_name && is_column_eligible(&f.field_type));
    if valid {
        Some(sort.to_string())
    } else {
        None
    }
}

/// Check if a field type is eligible for display as a list column.
pub(super) fn is_column_eligible(field_type: &FieldType) -> bool {
    matches!(
        field_type,
        FieldType::Text
            | FieldType::Email
            | FieldType::Number
            | FieldType::Select
            | FieldType::Checkbox
            | FieldType::Date
            | FieldType::Relationship
            | FieldType::Textarea
            | FieldType::Radio
            | FieldType::Upload
    )
}

/// Build a list URL preserving all query params (pagination, search, sort, filters).
pub(super) fn build_list_url(
    base: &str,
    page: i64,
    per_page: Option<i64>,
    search: Option<&str>,
    sort: Option<&str>,
    raw_where: &str,
) -> String {
    let mut url = format!("{}?page={}", base, page);
    if let Some(pp) = per_page {
        url.push_str(&format!("&per_page={}", pp));
    }
    if let Some(s) = search {
        url.push_str(&format!("&search={}", url_encode(s)));
    }
    if let Some(s) = sort {
        url.push_str(&format!("&sort={}", url_encode(s)));
    }
    // Preserve where params from original query string
    for part in raw_where.split('&') {
        if part.starts_with("where%5B") || part.starts_with("where[") {
            url.push('&');
            url.push_str(part);
        }
    }
    url
}

/// Simple percent-encoding for URL query values.
fn url_encode(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.' || b == b'~' {
                format!("{}", b as char)
            } else {
                format!("%{:02X}", b)
            }
        })
        .collect()
}

/// Extract only `where[...]` params from a raw query string (for pagination link preservation).
pub(super) fn extract_where_params(raw_query: &str) -> String {
    raw_query
        .split('&')
        .filter(|p| p.starts_with("where%5B") || p.starts_with("where["))
        .collect::<Vec<_>>()
        .join("&")
}

/// Compute a custom row label for an array or blocks row.
///
/// Priority: `row_label` Lua function > block-level `label_field` > field-level `label_field` > None.
pub(super) fn compute_row_label(
    admin: &crate::core::field::FieldAdmin,
    block_label_field: Option<&str>,
    row_data: Option<&serde_json::Map<String, serde_json::Value>>,
    hook_runner: &crate::hooks::lifecycle::HookRunner,
) -> Option<String> {
    // 1. Try row_label Lua function
    if let Some(ref func_ref) = admin.row_label {
        if let Some(row) = row_data {
            let json_val = serde_json::Value::Object(row.clone());
            if let Some(label) = hook_runner.call_row_label(func_ref, &json_val) {
                if !label.is_empty() {
                    return Some(label);
                }
            }
        }
    }

    // 2. Try block-level label_field, then field-level label_field
    let lf = block_label_field.or(admin.label_field.as_deref())?;
    let row = row_data?;
    let val = row.get(lf)?;
    let s = match val {
        serde_json::Value::String(s) if !s.is_empty() => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        _ => return None,
    };
    Some(s)
}

/// Check if the current locale is a non-default locale (fields should be locked).
pub(super) fn is_non_default_locale(state: &AdminState, requested_locale: Option<&str>) -> bool {
    let config = &state.config.locale;
    if !config.is_enabled() {
        return false;
    }
    let current = requested_locale.unwrap_or(&config.default_locale);
    current != config.default_locale
}

/// Map a `VersionSnapshot` to the JSON object used in templates.
pub(super) fn version_to_json(v: VersionSnapshot) -> serde_json::Value {
    serde_json::json!({
        "id": v.id,
        "version": v.version,
        "status": v.status,
        "latest": v.latest,
        "created_at": v.created_at,
    })
}

/// Fetch the last N versions + total count for sidebar display.
/// Returns `(versions_json, total_count)`.
pub(super) fn fetch_version_sidebar_data(
    pool: &DbPool,
    table_name: &str,
    parent_id: &str,
) -> (Vec<serde_json::Value>, i64) {
    if let Ok(conn) = pool.get() {
        let total = query::count_versions(&conn, table_name, parent_id).unwrap_or(0);
        let vers = query::list_versions(&conn, table_name, parent_id, Some(3), None)
            .unwrap_or_default()
            .into_iter()
            .map(version_to_json)
            .collect();
        (vers, total)
    } else {
        (vec![], 0)
    }
}

/// Execute the unpublish flow on an already-open transaction:
/// set status to draft, build snapshot, create version, prune.
pub(super) fn do_unpublish(
    tx: &rusqlite::Transaction,
    table_name: &str,
    parent_id: &str,
    fields: &[FieldDefinition],
    versions_config: Option<&VersionsConfig>,
    doc: &crate::core::Document,
) -> anyhow::Result<()> {
    query::set_document_status(tx, table_name, parent_id, "draft")?;
    let snapshot = query::build_snapshot(tx, table_name, fields, doc)?;
    query::create_version(tx, table_name, parent_id, "draft", &snapshot)?;
    if let Some(vc) = versions_config {
        if vc.max_versions > 0 {
            query::prune_versions(tx, table_name, parent_id, vc.max_versions)?;
        }
    }
    Ok(())
}

/// Translate validation errors using the translation system.
/// If a FieldError has a `key`, resolve it through `Translations::get_interpolated`;
/// otherwise use the raw English `message` (custom Lua validator messages).
pub(super) fn translate_validation_errors(
    ve: &ValidationError,
    translations: &Translations,
    locale: &str,
) -> HashMap<String, String> {
    ve.errors.iter()
        .map(|e| {
            let msg = if let Some(ref key) = e.key {
                translations.get_interpolated(locale, key, &e.params)
            } else {
                e.message.clone()
            };
            (e.field.clone(), msg)
        })
        .collect()
}

/// Render a 403 Forbidden page with the given message.
pub(super) fn forbidden(state: &AdminState, message: &str) -> (StatusCode, Html<String>) {
    let data = ContextBuilder::new(state, None)
        .page(PageType::Error403, "Forbidden")
        .set("message", serde_json::Value::String(message.to_string()))
        .build();
    let data = state.hook_runner.run_before_render(data);
    let html = match state.render("errors/403", &data) {
        Ok(html) => Html(html),
        Err(_) => Html(format!("<h1>403 Forbidden</h1><p>{}</p>", message)),
    };
    (StatusCode::FORBIDDEN, html)
}

/// Create a redirect response to the given URL (303 See Other).
pub(super) fn redirect_response(url: &str) -> axum::response::Response {
    Redirect::to(url).into_response()
}

/// Create an HTMX-aware redirect: returns 200 + `HX-Redirect` header so HTMX does a full
/// page navigation instead of an in-place body swap. This avoids issues with custom
/// elements (ProseMirror richtext editors in blocks) not re-initializing properly during
/// HTMX innerHTML swaps after write operations.
pub(super) fn htmx_redirect(url: &str) -> axum::response::Response {
    axum::response::Response::builder()
        .status(StatusCode::OK)
        .header("HX-Redirect", url)
        .body(axum::body::Body::empty())
        .unwrap_or_else(|_| Redirect::to(url).into_response())
}

/// Render a template and set the X-Crap-Toast header for client-side notifications.
pub(super) fn html_with_toast(state: &AdminState, template: &str, data: &serde_json::Value, toast: &str) -> axum::response::Response {
    match state.render(template, data) {
        Ok(html) => {
            let mut resp = Html(html).into_response();
            let json_toast = serde_json::json!({ "message": toast, "type": "error" }).to_string();
            if let Ok(val) = json_toast.parse() {
                resp.headers_mut().insert("X-Crap-Toast", val);
            }
            resp
        }
        Err(e) => {
            tracing::error!("Template render error: {}", e);
            Html("<h1>Something went wrong</h1><p>Please try again.</p>".to_string()).into_response()
        }
    }
}

/// Render a template, falling back to a plain error page on failure.
pub(super) fn render_or_error(state: &AdminState, template: &str, data: &serde_json::Value) -> Html<String> {
    match state.render(template, data) {
        Ok(html) => Html(html),
        Err(e) => {
            tracing::error!("Template render error: {}", e);
            Html("<h1>Something went wrong</h1><p>Please try again.</p>".to_string())
        }
    }
}

/// Render a 404 Not Found page with the given message.
pub(super) fn not_found(state: &AdminState, message: &str) -> (StatusCode, Html<String>) {
    let data = ContextBuilder::new(state, None)
        .page(PageType::Error404, "Not Found")
        .set("message", serde_json::Value::String(message.to_string()))
        .build();
    let data = state.hook_runner.run_before_render(data);
    let html = match state.render("errors/404", &data) {
        Ok(html) => Html(html),
        Err(_) => Html(format!("<h1>404</h1><p>{}</p>", message)),
    };
    (StatusCode::NOT_FOUND, html)
}

/// Render a 500 Internal Server Error page with the given message.
pub(super) fn server_error(state: &AdminState, message: &str) -> (StatusCode, Html<String>) {
    let data = ContextBuilder::new(state, None)
        .page(PageType::Error500, "Server Error")
        .set("message", serde_json::Value::String(message.to_string()))
        .build();
    let data = state.hook_runner.run_before_render(data);
    let html = match state.render("errors/500", &data) {
        Ok(html) => Html(html),
        Err(_) => Html(format!("<h1>500</h1><p>{}</p>", message)),
    };
    (StatusCode::INTERNAL_SERVER_ERROR, html)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::collection::*;

    fn test_def() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("posts");
        def.timestamps = true;
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("status", FieldType::Select).build(),
            FieldDefinition::builder("body", FieldType::Richtext).build(),
            FieldDefinition::builder("count", FieldType::Number).build(),
        ];
        def
    }

    // --- parse_where_params tests ---

    #[test]
    fn parse_where_empty_query() {
        let def = test_def();
        let result = parse_where_params("", &def);
        assert!(result.is_empty());
    }

    #[test]
    fn parse_where_equals_filter() {
        let def = test_def();
        let result = parse_where_params("where[title][equals]=hello", &def);
        assert_eq!(result.len(), 1);
        match &result[0] {
            FilterClause::Single(f) => {
                assert_eq!(f.field, "title");
                assert!(matches!(&f.op, FilterOp::Equals(v) if v == "hello"));
            }
            _ => panic!("Expected Single filter"),
        }
    }

    #[test]
    fn parse_where_multiple_filters() {
        let def = test_def();
        let result = parse_where_params("where[title][contains]=foo&where[count][gt]=5", &def);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn parse_where_invalid_field_ignored() {
        let def = test_def();
        let result = parse_where_params("where[nonexistent][equals]=foo", &def);
        assert!(result.is_empty());
    }

    #[test]
    fn parse_where_invalid_op_ignored() {
        let def = test_def();
        let result = parse_where_params("where[title][invalid]=foo", &def);
        assert!(result.is_empty());
    }

    #[test]
    fn parse_where_system_column() {
        let def = test_def();
        let result = parse_where_params("where[created_at][gt]=2024-01-01", &def);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn parse_where_exists_op() {
        let def = test_def();
        let result = parse_where_params("where[title][exists]=", &def);
        assert_eq!(result.len(), 1);
        match &result[0] {
            FilterClause::Single(f) => assert!(matches!(f.op, FilterOp::Exists)),
            _ => panic!("Expected Single"),
        }
    }

    #[test]
    fn parse_where_encoded_value() {
        let def = test_def();
        let result = parse_where_params("where[title][equals]=hello%20world", &def);
        assert_eq!(result.len(), 1);
        match &result[0] {
            FilterClause::Single(f) => {
                assert!(matches!(&f.op, FilterOp::Equals(v) if v == "hello world"));
            }
            _ => panic!("Expected Single"),
        }
    }

    // --- validate_sort tests ---

    #[test]
    fn validate_sort_valid_field() {
        let def = test_def();
        assert_eq!(validate_sort("title", &def), Some("title".to_string()));
    }

    #[test]
    fn validate_sort_descending() {
        let def = test_def();
        assert_eq!(validate_sort("-title", &def), Some("-title".to_string()));
    }

    #[test]
    fn validate_sort_system_col() {
        let def = test_def();
        assert_eq!(validate_sort("-created_at", &def), Some("-created_at".to_string()));
    }

    #[test]
    fn validate_sort_invalid() {
        let def = test_def();
        assert_eq!(validate_sort("nonexistent", &def), None);
    }

    #[test]
    fn validate_sort_ineligible_field() {
        let def = test_def();
        // body is Richtext — not column-eligible
        assert_eq!(validate_sort("body", &def), None);
    }

    // --- build_list_url tests ---

    #[test]
    fn build_list_url_basic() {
        let url = build_list_url("/admin/collections/posts", 2, None, None, None, "");
        assert_eq!(url, "/admin/collections/posts?page=2");
    }

    #[test]
    fn build_list_url_with_search_sort() {
        let url = build_list_url("/admin/collections/posts", 1, None, Some("hello"), Some("-title"), "");
        assert!(url.contains("search=hello"));
        assert!(url.contains("sort=-title"));
    }

    #[test]
    fn build_list_url_preserves_where() {
        let url = build_list_url(
            "/admin/collections/posts", 1, None, None, None,
            "where[title][equals]=foo&page=1",
        );
        assert!(url.contains("where[title][equals]=foo"));
        assert!(!url.contains("page=1&page=1")); // should not duplicate page
    }

    // --- is_column_eligible tests ---

    #[test]
    fn column_eligible_text() {
        assert!(is_column_eligible(&FieldType::Text));
        assert!(is_column_eligible(&FieldType::Email));
        assert!(is_column_eligible(&FieldType::Number));
        assert!(is_column_eligible(&FieldType::Select));
        assert!(is_column_eligible(&FieldType::Checkbox));
        assert!(is_column_eligible(&FieldType::Date));
    }

    #[test]
    fn column_ineligible_richtext() {
        assert!(!is_column_eligible(&FieldType::Richtext));
        assert!(!is_column_eligible(&FieldType::Array));
        assert!(!is_column_eligible(&FieldType::Group));
        assert!(!is_column_eligible(&FieldType::Blocks));
        assert!(!is_column_eligible(&FieldType::Json));
        assert!(!is_column_eligible(&FieldType::Code));
        assert!(!is_column_eligible(&FieldType::Join));
    }

    // --- url_decode tests ---

    #[test]
    fn url_decode_basic() {
        assert_eq!(url_decode("hello%20world"), "hello world");
        assert_eq!(url_decode("foo+bar"), "foo bar");
        assert_eq!(url_decode("plain"), "plain");
    }

    // --- auto_label_from_name tests ---

    #[test]
    fn auto_label_underscore_separated() {
        assert_eq!(auto_label_from_name("my_field"), "My Field");
    }

    #[test]
    fn auto_label_single_word() {
        assert_eq!(auto_label_from_name("title"), "Title");
    }

    #[test]
    fn auto_label_empty_string() {
        assert_eq!(auto_label_from_name(""), "");
    }

    #[test]
    fn auto_label_multiple_words() {
        assert_eq!(auto_label_from_name("created_at"), "Created At");
    }

    #[test]
    fn auto_label_double_underscore() {
        assert_eq!(auto_label_from_name("seo__title"), "Seo  Title");
    }

    // --- strip_denied_fields tests ---

    #[test]
    fn strip_denied_fields_removes_specified_keys() {
        let mut fields = HashMap::new();
        fields.insert("title".to_string(), serde_json::json!("Hello"));
        fields.insert("secret".to_string(), serde_json::json!("hidden"));
        fields.insert("body".to_string(), serde_json::json!("content"));

        strip_denied_fields(&mut fields, &["secret".to_string()]);

        assert_eq!(fields.len(), 2);
        assert!(fields.contains_key("title"));
        assert!(fields.contains_key("body"));
        assert!(!fields.contains_key("secret"));
    }

    #[test]
    fn strip_denied_fields_empty_denied_list() {
        let mut fields = HashMap::new();
        fields.insert("title".to_string(), serde_json::json!("Hello"));
        fields.insert("body".to_string(), serde_json::json!("content"));

        strip_denied_fields(&mut fields, &[]);

        assert_eq!(fields.len(), 2);
        assert!(fields.contains_key("title"));
        assert!(fields.contains_key("body"));
    }

    #[test]
    fn strip_denied_fields_empty_fields_map() {
        let mut fields: HashMap<String, serde_json::Value> = HashMap::new();
        strip_denied_fields(&mut fields, &["secret".to_string()]);
        assert!(fields.is_empty());
    }

    #[test]
    fn strip_denied_fields_nonexistent_key() {
        let mut fields = HashMap::new();
        fields.insert("title".to_string(), serde_json::json!("Hello"));

        strip_denied_fields(&mut fields, &["nonexistent".to_string()]);

        assert_eq!(fields.len(), 1);
        assert!(fields.contains_key("title"));
    }

    // --- version_to_json tests ---

    #[test]
    fn version_to_json_maps_all_fields() {
        let v = VersionSnapshot::builder("v1", "doc1")
            .version(3)
            .status("published")
            .latest(true)
            .created_at("2026-01-01T00:00:00Z")
            .updated_at("2026-01-01T00:00:00Z")
            .snapshot(serde_json::json!({}))
            .build();
        let json = version_to_json(v);
        assert_eq!(json["id"], "v1");
        assert_eq!(json["version"], 3);
        assert_eq!(json["status"], "published");
        assert_eq!(json["latest"], true);
        assert_eq!(json["created_at"], "2026-01-01T00:00:00Z");
    }

    // --- compute_row_label tests ---

    use crate::core::field::FieldAdmin;

    #[test]
    fn compute_row_label_from_label_field() {
        let admin = FieldAdmin::builder().label_field("title").build();
        let mut row = serde_json::Map::new();
        row.insert("title".to_string(), serde_json::json!("My Title"));
        // Construct a minimal mock HookRunner -- compute_row_label with no row_label set
        // will skip the Lua call and go straight to label_field lookup.
        // Since we can't construct a real HookRunner in a unit test, we test the label_field
        // and block_label_field paths only (they don't need Lua).

        // Direct test of label value extraction logic (matching compute_row_label's inner logic)
        let lf = admin.label_field.as_deref();
        assert_eq!(lf, Some("title"));
        let val = row.get("title").unwrap();
        match val {
            serde_json::Value::String(s) if !s.is_empty() => {
                assert_eq!(s, "My Title");
            }
            _ => panic!("Expected non-empty string"),
        }
    }

    #[test]
    fn compute_row_label_number_value() {
        // Test that Number values are stringified
        let val = serde_json::json!(42);
        match &val {
            serde_json::Value::Number(n) => assert_eq!(n.to_string(), "42"),
            _ => panic!("Expected number"),
        }
    }

    #[test]
    fn compute_row_label_bool_value() {
        // Test that Bool values are stringified
        let val = serde_json::json!(true);
        match &val {
            serde_json::Value::Bool(b) => assert_eq!(b.to_string(), "true"),
            _ => panic!("Expected bool"),
        }
    }

    // --- htmx_redirect tests ---

    #[test]
    fn htmx_redirect_returns_200_with_header() {
        let resp = htmx_redirect("/admin/collections/posts");
        assert_eq!(resp.status(), StatusCode::OK);
        let hx = resp.headers().get("HX-Redirect").unwrap();
        assert_eq!(hx, "/admin/collections/posts");
    }

    // --- redirect_response tests ---

    #[test]
    fn redirect_response_returns_303() {
        let resp = redirect_response("/admin/collections");
        assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    }

    // --- extract_editor_locale tests ---

    fn locale_config_enabled() -> crate::config::LocaleConfig {
        crate::config::LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string(), "fr".to_string()],
            fallback: false,
        }
    }

    #[test]
    fn extract_editor_locale_from_cookie() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(axum::http::header::COOKIE, "crap_editor_locale=de".parse().unwrap());
        let result = extract_editor_locale(&headers, &locale_config_enabled());
        assert_eq!(result, Some("de".to_string()));
    }

    #[test]
    fn extract_editor_locale_falls_back_to_default() {
        let headers = axum::http::HeaderMap::new();
        let result = extract_editor_locale(&headers, &locale_config_enabled());
        assert_eq!(result, Some("en".to_string()));
    }

    #[test]
    fn extract_editor_locale_invalid_locale_falls_back() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(axum::http::header::COOKIE, "crap_editor_locale=zz".parse().unwrap());
        let result = extract_editor_locale(&headers, &locale_config_enabled());
        assert_eq!(result, Some("en".to_string()));
    }

    #[test]
    fn extract_editor_locale_disabled_returns_none() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(axum::http::header::COOKIE, "crap_editor_locale=de".parse().unwrap());
        let config = crate::config::LocaleConfig::default(); // empty locales = disabled
        let result = extract_editor_locale(&headers, &config);
        assert_eq!(result, None);
    }

    #[test]
    fn extract_editor_locale_with_multiple_cookies() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(axum::http::header::COOKIE, "crap_session=abc; crap_editor_locale=fr; other=xyz".parse().unwrap());
        let result = extract_editor_locale(&headers, &locale_config_enabled());
        assert_eq!(result, Some("fr".to_string()));
    }

    // --- translate_validation_errors tests ---

    use crate::core::validate::FieldError;

    fn test_translations() -> Translations {
        Translations::load(std::path::Path::new("/nonexistent"))
    }

    #[test]
    fn translate_with_key_uses_translation() {
        let translations = test_translations();
        let mut params = HashMap::new();
        params.insert("field".to_string(), "Title".to_string());
        let ve = ValidationError::new(vec![
            FieldError::with_key("title", "title is required", "validation.required", params),
        ]);
        let map = translate_validation_errors(&ve, &translations, "en");
        assert_eq!(map.get("title").unwrap(), "Title is required");
    }

    #[test]
    fn translate_without_key_uses_raw_message() {
        let translations = test_translations();
        let ve = ValidationError::new(vec![
            FieldError::new("title", "custom lua error"),
        ]);
        let map = translate_validation_errors(&ve, &translations, "en");
        assert_eq!(map.get("title").unwrap(), "custom lua error");
    }

    #[test]
    fn translate_german_locale() {
        let translations = test_translations();
        let mut params = HashMap::new();
        params.insert("field".to_string(), "Titel".to_string());
        let ve = ValidationError::new(vec![
            FieldError::with_key("title", "title is required", "validation.required", params),
        ]);
        let map = translate_validation_errors(&ve, &translations, "de");
        assert_eq!(map.get("title").unwrap(), "Titel ist erforderlich");
    }
}
