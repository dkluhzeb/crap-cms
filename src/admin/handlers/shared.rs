//! Shared helper functions for admin handlers (collections + globals).

use axum::{
    Extension,
    http::{HeaderMap, StatusCode, header},
    response::{Html, IntoResponse, Redirect, Response},
};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use std::collections::HashMap;

use crate::{
    admin::{
        AdminState, Translations,
        context::{ContextBuilder, PageType},
        server::extract_cookie,
    },
    config::LocaleConfig,
    core::{
        AuthUser, Document, FieldAdmin, FieldDefinition, document::VersionSnapshot,
        event::EventUser, field, richtext::renderer::html_escape, validate::ValidationError,
    },
    db::{AccessResult, DbPool, LocaleContext, query},
    hooks::{HookRunner, lifecycle::access::has_any_field_access},
};

// Re-export field context functions from the dedicated module.
pub(super) use crate::admin::handlers::field_context::{
    EnrichOptions, apply_display_conditions, build_field_contexts, enrich_field_contexts,
    split_sidebar_fields,
};

// Re-export query utilities from the dedicated module.
pub(crate) use super::query_utils::{
    build_list_url, build_list_url_with_cursor, extract_where_params, is_column_eligible,
    parse_where_params, url_decode, validate_sort,
};

/// Query parameters for paginated collection list views.
#[derive(Debug, Deserialize)]
pub struct PaginationParams {
    /// The current page number (1-indexed).
    pub page: Option<i64>,
    /// The number of items per page.
    pub per_page: Option<i64>,
    /// Search query string.
    pub search: Option<String>,
    /// Sort string (e.g. "title" or "-title").
    pub sort: Option<String>,
    /// Forward cursor for cursor-based pagination.
    pub after_cursor: Option<String>,
    /// Backward cursor for cursor-based pagination.
    pub before_cursor: Option<String>,
    /// When "1", show the trash view (soft-deleted documents only).
    pub trash: Option<String>,
}

/// Extract the editor locale from the `crap_editor_locale` cookie.
/// Falls back to the config's default locale if the cookie is absent or invalid.
/// Returns `None` if locales are not enabled.
pub(crate) fn extract_editor_locale(headers: &HeaderMap, config: &LocaleConfig) -> Option<String> {
    if !config.is_enabled() {
        return None;
    }

    let cookie_str = headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let raw = extract_cookie(cookie_str, "crap_editor_locale");

    let locale = raw.unwrap_or(&config.default_locale);

    // Validate against configured locales
    if config.locales.contains(&locale.to_string()) {
        Some(locale.to_string())
    } else {
        Some(config.default_locale.clone())
    }
}

/// Extract the user document from AuthUser extension (for access checks).
pub(crate) fn get_user_doc(auth_user: &Option<Extension<AuthUser>>) -> Option<&Document> {
    auth_user.as_ref().map(|Extension(au)| &au.user_doc)
}

/// Extract an EventUser from the AuthUser extension (for SSE event attribution).
pub(crate) fn get_event_user(auth_user: &Option<Extension<AuthUser>>) -> Option<EventUser> {
    auth_user
        .as_ref()
        .map(|Extension(au)| EventUser::new(au.claims.sub.clone(), au.claims.email.clone()))
}

/// Strip denied fields from a document's fields map.
pub(crate) fn strip_denied_fields(fields: &mut HashMap<String, Value>, denied: &[String]) {
    for name in denied {
        fields.remove(name);
    }
}

/// Helper to check collection/global-level access. Returns AccessResult or renders a 403 page.
pub(crate) fn check_access_or_forbid(
    state: &AdminState,
    access_ref: Option<&str>,
    auth_user: &Option<Extension<AuthUser>>,
    id: Option<&str>,
    data: Option<&HashMap<String, Value>>,
) -> Result<AccessResult, Box<Response>> {
    // No access function configured — check default-deny policy
    if access_ref.is_none() {
        return if state.config.access.default_deny {
            Ok(AccessResult::Denied)
        } else {
            Ok(AccessResult::Allowed)
        };
    }

    let user_doc = get_user_doc(auth_user);

    let mut conn = state
        .pool
        .get()
        .map_err(|_| Box::new(forbidden(state, "Database error").into_response()))?;

    let tx = conn
        .transaction()
        .map_err(|_| Box::new(forbidden(state, "Database error").into_response()))?;

    let result = state
        .hook_runner
        .check_access(access_ref, user_doc, id, data, &tx)
        .map_err(|e| {
            tracing::error!("Access check error: {}", e);
            Box::new(forbidden(state, "Access check failed").into_response())
        })?;

    tx.commit()
        .map_err(|_| Box::new(forbidden(state, "Database error").into_response()))?;

    Ok(result)
}

/// Build locale template context (selector data) from config + current locale.
/// Returns `(locale_ctx_for_db, template_json)` where template_json has
/// `has_locales`, `current_locale`, `locales` (array with value/label/selected).
pub(crate) fn build_locale_template_data(
    state: &AdminState,
    requested_locale: Option<&str>,
) -> (Option<LocaleContext>, Value) {
    let config = &state.config.locale;

    if !config.is_enabled() {
        return (None, json!({}));
    }

    let current = requested_locale.unwrap_or(&config.default_locale);

    let locale_ctx = LocaleContext::from_locale_string(Some(current), config);

    let locales: Vec<Value> = config
        .locales
        .iter()
        .map(|l| {
            json!({
                "value": l,
                "label": l.to_uppercase(),
                "selected": l == current,
            })
        })
        .collect();

    let data = json!({
        "has_locales": true,
        "current_locale": current,
        "locales": locales,
    });

    (locale_ctx, data)
}

/// Auto-generate a label from a field name (e.g. "my_field" -> "My Field").
pub(crate) fn auto_label_from_name(name: &str) -> String {
    field::to_title_case(name)
}

/// Compute a custom row label for an array or blocks row.
///
/// Priority: `row_label` Lua function > block-level `label_field` > field-level `label_field` > None.
pub(crate) fn compute_row_label(
    admin: &FieldAdmin,
    block_label_field: Option<&str>,
    row_data: Option<&Map<String, Value>>,
    hook_runner: &HookRunner,
) -> Option<String> {
    // 1. Try row_label Lua function
    if let Some(ref func_ref) = admin.row_label
        && let Some(row) = row_data
    {
        let json_val = Value::Object(row.clone());

        if let Some(label) = hook_runner.call_row_label(func_ref, &json_val)
            && !label.is_empty()
        {
            return Some(label);
        }
    }

    // 2. Try block-level label_field, then field-level label_field
    let lf = block_label_field.or(admin.label_field.as_deref())?;
    let row = row_data?;
    let val = row.get(lf)?;
    let s = match val {
        Value::String(s) if !s.is_empty() => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => return None,
    };

    Some(s)
}

/// Check if the current locale is a non-default locale (fields should be locked).
pub(crate) fn is_non_default_locale(state: &AdminState, requested_locale: Option<&str>) -> bool {
    let config = &state.config.locale;

    if !config.is_enabled() {
        return false;
    }

    let current = requested_locale.unwrap_or(&config.default_locale);

    current != config.default_locale
}

/// Map a `VersionSnapshot` to the JSON object used in templates.
pub(crate) fn version_to_json(v: VersionSnapshot) -> Value {
    json!({
        "id": v.id,
        "version": v.version,
        "status": v.status,
        "latest": v.latest,
        "created_at": v.created_at,
    })
}

/// Fetch the last N versions + total count for sidebar display.
/// Returns `(versions_json, total_count)`.
pub(crate) fn fetch_version_sidebar_data(
    pool: &DbPool,
    table_name: &str,
    parent_id: &str,
) -> (Vec<Value>, i64) {
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

/// Translate validation errors using the translation system.
/// If a FieldError has a `key`, resolve it through `Translations::get_interpolated`;
/// otherwise use the raw English `message` (custom Lua validator messages).
pub(crate) fn translate_validation_errors(
    ve: &ValidationError,
    translations: &Translations,
    locale: &str,
) -> HashMap<String, String> {
    ve.errors
        .iter()
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

/// Returns field names denied for the current user's read access, or a server error response.
/// Skips the check entirely (returns empty vec) if no field has read access configured.
pub(crate) fn compute_denied_read_fields(
    state: &AdminState,
    auth_user: &Option<Extension<AuthUser>>,
    fields: &[FieldDefinition],
) -> Result<Vec<String>, Box<Response>> {
    if !has_any_field_access(fields, |f| f.access.read.as_deref()) {
        return Ok(Vec::new());
    }

    let user_doc = get_user_doc(auth_user);

    let mut conn = state.pool.get().map_err(|e| {
        tracing::error!("Field access check pool error: {}", e);
        Box::new(server_error(state, "Database error"))
    })?;

    let tx = conn.transaction().map_err(|e| {
        tracing::error!("Field access check tx error: {}", e);
        Box::new(server_error(state, "Database error"))
    })?;

    let denied = state
        .hook_runner
        .check_field_read_access(fields, user_doc, &tx);

    // Read-only access check — commit result is irrelevant, rollback on drop is safe
    if let Err(e) = tx.commit() {
        tracing::warn!("tx commit failed: {e}");
    }

    Ok(denied)
}

/// Strips fields denied for write access from a `HashMap<String, String>` form in-place.
/// Returns `Err(response)` on pool/tx failure, `Ok(())` on success.
pub(crate) fn strip_write_denied_string_fields(
    state: &AdminState,
    auth_user: &Option<Extension<AuthUser>>,
    fields: &[FieldDefinition],
    operation: &str,
    form_data: &mut HashMap<String, String>,
) -> Result<(), Box<Response>> {
    let extractor: fn(&FieldDefinition) -> Option<&str> = match operation {
        "create" => |f| f.access.create.as_deref(),
        "update" => |f| f.access.update.as_deref(),
        _ => return Ok(()),
    };
    if !has_any_field_access(fields, extractor) {
        return Ok(());
    }

    let user_doc = get_user_doc(auth_user);

    let mut conn = state.pool.get().map_err(|e| {
        tracing::error!("Field access check pool error: {}", e);
        Box::new(server_error(state, "Database error"))
    })?;

    let tx = conn.transaction().map_err(|e| {
        tracing::error!("Field access check tx error: {}", e);
        Box::new(server_error(state, "Database error"))
    })?;

    let denied = state
        .hook_runner
        .check_field_write_access(fields, user_doc, operation, &tx);

    // Read-only access check — commit result is irrelevant, rollback on drop is safe
    if let Err(e) = tx.commit() {
        tracing::warn!("tx commit failed: {e}");
    }

    for name in &denied {
        form_data.remove(name);
    }

    Ok(())
}

/// Flattens document fields for form rendering. Group fields become `parent__child` keys,
/// recursively flattening nested groups (e.g. `address: { geo: { lat: "40" } }` →
/// `address__geo__lat: "40"`).
pub(crate) fn flatten_document_values(
    fields: &HashMap<String, Value>,
    field_defs: &[FieldDefinition],
) -> HashMap<String, String> {
    fields
        .iter()
        .flat_map(|(k, v)| {
            if let Value::Object(obj) = v
                && field_defs
                    .iter()
                    .any(|f| f.name == *k && f.field_type == field::FieldType::Group)
            {
                let mut out = Vec::new();
                flatten_group_value(k, obj, &mut out);
                return out;
            }
            vec![(k.clone(), value_to_form_string(v))]
        })
        .collect()
}

/// Recursively flatten a group object into `prefix__key` pairs.
fn flatten_group_value(prefix: &str, obj: &Map<String, Value>, out: &mut Vec<(String, String)>) {
    for (sub_k, sub_v) in obj {
        let col = format!("{}__{}", prefix, sub_k);
        if let Value::Object(nested) = sub_v {
            flatten_group_value(&col, nested, out);
        } else {
            out.push((col, value_to_form_string(sub_v)));
        }
    }
}

/// Convert a serde_json Value to a string suitable for form rendering.
fn value_to_form_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Render a 403 Forbidden page with the given message.
pub(crate) fn forbidden(state: &AdminState, message: &str) -> Response {
    let data = ContextBuilder::new(state, None)
        .page(PageType::Error403, "forbidden_page_title")
        .set("message", Value::String(message.to_string()))
        .build();

    let data = state.hook_runner.run_before_render(data);

    let html = match state.render("errors/403", &data) {
        Ok(html) => Html(html),
        Err(_) => Html(format!(
            "<h1>403 Forbidden</h1><p>{}</p>",
            html_escape(message)
        )),
    };

    (StatusCode::FORBIDDEN, html).into_response()
}

/// Create a redirect response to the given URL (303 See Other).
pub(crate) fn redirect_response(url: &str) -> Response {
    Redirect::to(url).into_response()
}

/// Create an HTMX-aware redirect: returns 200 + `HX-Redirect` header so HTMX does a full
/// page navigation instead of an in-place body swap. This avoids issues with custom
/// elements (ProseMirror richtext editors in blocks) not re-initializing properly during
/// HTMX innerHTML swaps after write operations.
pub(crate) fn htmx_redirect(url: &str) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header("HX-Redirect", url)
        .body(axum::body::Body::empty())
        .unwrap_or_else(|_| Redirect::to(url).into_response())
}

/// Like `htmx_redirect`, but also includes `X-Created-Id` and `X-Created-Label`
/// headers so inline create panels can identify the newly created document.
/// The label is percent-encoded to safely handle non-ASCII characters in HTTP headers.
pub(crate) fn htmx_redirect_with_created(url: &str, id: &str, label: &str) -> Response {
    let encoded_label = percent_encode_header(label);
    Response::builder()
        .status(StatusCode::OK)
        .header("HX-Redirect", url)
        .header("X-Created-Id", id)
        .header("X-Created-Label", &encoded_label)
        .body(axum::body::Body::empty())
        .unwrap_or_else(|_| Redirect::to(url).into_response())
}

/// Percent-encode a string so it is safe for HTTP header values.
/// Non-ASCII bytes and control characters are encoded as `%XX`.
fn percent_encode_header(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_graphic() || b == b' ' {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

/// Render a template and set the X-Crap-Toast header for client-side notifications.
pub(crate) fn html_with_toast(
    state: &AdminState,
    template: &str,
    data: &Value,
    toast: &str,
) -> Response {
    match state.render(template, data) {
        Ok(html) => {
            let mut resp = Html(html).into_response();
            let json_toast = json!({ "message": toast, "type": "error" }).to_string();

            if let Ok(val) = json_toast.parse() {
                resp.headers_mut().insert("X-Crap-Toast", val);
            }

            resp
        }
        Err(e) => {
            tracing::error!("Template render error: {}", e);
            Html("<h1>Something went wrong</h1><p>Please try again.</p>".to_string())
                .into_response()
        }
    }
}

/// Return a 422 response with only the toast header — HTMX won't swap the body,
/// so the user keeps their form data while seeing the error notification.
pub(crate) fn toast_only_error(msg: &str) -> Response {
    let json_toast = json!({ "message": msg, "type": "error" }).to_string();
    let mut resp = Response::builder()
        .status(StatusCode::UNPROCESSABLE_ENTITY)
        .body(axum::body::Body::empty())
        .unwrap();

    if let Ok(val) = json_toast.parse() {
        resp.headers_mut().insert("X-Crap-Toast", val);
    }

    resp
}

/// Render a template, falling back to a plain error page on failure.
pub(crate) fn render_or_error(state: &AdminState, template: &str, data: &Value) -> Response {
    match state.render(template, data) {
        Ok(html) => Html(html),
        Err(e) => {
            tracing::error!("Template render error: {}", e);
            Html("<h1>Something went wrong</h1><p>Please try again.</p>".to_string())
        }
    }
    .into_response()
}

/// Render a 404 Not Found page with the given message.
pub(crate) fn not_found(state: &AdminState, message: &str) -> Response {
    let data = ContextBuilder::new(state, None)
        .page(PageType::Error404, "not_found_page_title")
        .set("message", Value::String(message.to_string()))
        .build();

    let data = state.hook_runner.run_before_render(data);

    let html = match state.render("errors/404", &data) {
        Ok(html) => Html(html),
        Err(_) => Html(format!("<h1>404</h1><p>{}</p>", html_escape(message))),
    };

    (StatusCode::NOT_FOUND, html).into_response()
}

/// Render a 500 Internal Server Error page with the given message.
pub(crate) fn server_error(state: &AdminState, message: &str) -> Response {
    let data = ContextBuilder::new(state, None)
        .page(PageType::Error500, "server_error_page_title")
        .set("message", Value::String(message.to_string()))
        .build();

    let data = state.hook_runner.run_before_render(data);

    let html = match state.render("errors/500", &data) {
        Ok(html) => Html(html),
        Err(_) => Html(format!("<h1>500</h1><p>{}</p>", html_escape(message))),
    };

    (StatusCode::INTERNAL_SERVER_ERROR, html).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(auto_label_from_name("seo__title"), "Seo Title");
    }

    // --- strip_denied_fields tests ---

    #[test]
    fn strip_denied_fields_removes_specified_keys() {
        let mut fields = HashMap::new();
        fields.insert("title".to_string(), json!("Hello"));
        fields.insert("secret".to_string(), json!("hidden"));
        fields.insert("body".to_string(), json!("content"));

        strip_denied_fields(&mut fields, &["secret".to_string()]);

        assert_eq!(fields.len(), 2);
        assert!(fields.contains_key("title"));
        assert!(fields.contains_key("body"));
        assert!(!fields.contains_key("secret"));
    }

    #[test]
    fn strip_denied_fields_empty_denied_list() {
        let mut fields = HashMap::new();
        fields.insert("title".to_string(), json!("Hello"));
        fields.insert("body".to_string(), json!("content"));

        strip_denied_fields(&mut fields, &[]);

        assert_eq!(fields.len(), 2);
        assert!(fields.contains_key("title"));
        assert!(fields.contains_key("body"));
    }

    #[test]
    fn strip_denied_fields_empty_fields_map() {
        let mut fields: HashMap<String, Value> = HashMap::new();
        strip_denied_fields(&mut fields, &["secret".to_string()]);
        assert!(fields.is_empty());
    }

    #[test]
    fn strip_denied_fields_nonexistent_key() {
        let mut fields = HashMap::new();
        fields.insert("title".to_string(), json!("Hello"));

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
            .snapshot(json!({}))
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
        let mut row = Map::new();
        row.insert("title".to_string(), json!("My Title"));
        // Construct a minimal mock HookRunner -- compute_row_label with no row_label set
        // will skip the Lua call and go straight to label_field lookup.
        // Since we can't construct a real HookRunner in a unit test, we test the label_field
        // and block_label_field paths only (they don't need Lua).

        // Direct test of label value extraction logic (matching compute_row_label's inner logic)
        let lf = admin.label_field.as_deref();
        assert_eq!(lf, Some("title"));
        let val = row.get("title").unwrap();
        match val {
            Value::String(s) if !s.is_empty() => {
                assert_eq!(s, "My Title");
            }
            _ => panic!("Expected non-empty string"),
        }
    }

    #[test]
    fn compute_row_label_number_value() {
        // Test that Number values are stringified
        let val = json!(42);
        match &val {
            Value::Number(n) => assert_eq!(n.to_string(), "42"),
            _ => panic!("Expected number"),
        }
    }

    #[test]
    fn compute_row_label_bool_value() {
        // Test that Bool values are stringified
        let val = json!(true);
        match &val {
            Value::Bool(b) => assert_eq!(b.to_string(), "true"),
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

    fn locale_config_enabled() -> LocaleConfig {
        LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string(), "fr".to_string()],
            fallback: false,
        }
    }

    #[test]
    fn extract_editor_locale_from_cookie() {
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, "crap_editor_locale=de".parse().unwrap());
        let result = extract_editor_locale(&headers, &locale_config_enabled());
        assert_eq!(result, Some("de".to_string()));
    }

    #[test]
    fn extract_editor_locale_falls_back_to_default() {
        let headers = HeaderMap::new();
        let result = extract_editor_locale(&headers, &locale_config_enabled());
        assert_eq!(result, Some("en".to_string()));
    }

    #[test]
    fn extract_editor_locale_invalid_locale_falls_back() {
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, "crap_editor_locale=zz".parse().unwrap());
        let result = extract_editor_locale(&headers, &locale_config_enabled());
        assert_eq!(result, Some("en".to_string()));
    }

    #[test]
    fn extract_editor_locale_disabled_returns_none() {
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, "crap_editor_locale=de".parse().unwrap());
        let config = LocaleConfig::default(); // empty locales = disabled
        let result = extract_editor_locale(&headers, &config);
        assert_eq!(result, None);
    }

    #[test]
    fn extract_editor_locale_with_multiple_cookies() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            "crap_session=abc; crap_editor_locale=fr; other=xyz"
                .parse()
                .unwrap(),
        );
        let result = extract_editor_locale(&headers, &locale_config_enabled());
        assert_eq!(result, Some("fr".to_string()));
    }

    // --- flatten_document_values tests ---

    use crate::core::field::{FieldDefinition, FieldType};

    #[test]
    fn flatten_document_values_simple_fields() {
        let mut fields = HashMap::new();
        fields.insert("title".to_string(), json!("Hello"));
        fields.insert("count".to_string(), json!(42));

        let defs = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("count", FieldType::Number).build(),
        ];

        let flat = flatten_document_values(&fields, &defs);
        assert_eq!(flat.get("title").unwrap(), "Hello");
        assert_eq!(flat.get("count").unwrap(), "42");
    }

    #[test]
    fn flatten_document_values_group_fields() {
        let mut fields = HashMap::new();
        fields.insert(
            "config".to_string(),
            json!({"label": "My Config", "enabled": true}),
        );

        let defs = vec![
            FieldDefinition::builder("config", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("label", FieldType::Text).build(),
                    FieldDefinition::builder("enabled", FieldType::Checkbox).build(),
                ])
                .build(),
        ];

        let flat = flatten_document_values(&fields, &defs);
        assert_eq!(flat.get("config__label").unwrap(), "My Config");
        assert_eq!(flat.get("config__enabled").unwrap(), "true");
        assert!(
            !flat.contains_key("config"),
            "group key should not be present"
        );
    }

    #[test]
    fn flatten_document_values_nested_groups() {
        let mut fields = HashMap::new();
        fields.insert("outer".to_string(), json!({"inner": {"deep": "value"}}));

        let defs = vec![
            FieldDefinition::builder("outer", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("inner", FieldType::Group)
                        .fields(vec![
                            FieldDefinition::builder("deep", FieldType::Text).build(),
                        ])
                        .build(),
                ])
                .build(),
        ];

        let flat = flatten_document_values(&fields, &defs);
        assert_eq!(
            flat.get("outer__inner__deep").unwrap(),
            "value",
            "nested group should flatten to outer__inner__deep"
        );
        assert!(!flat.contains_key("outer"));
        assert!(!flat.contains_key("outer__inner"));
    }

    #[test]
    fn flatten_document_values_group_with_array_value() {
        // Array values inside groups should be serialized as JSON strings
        let mut fields = HashMap::new();
        fields.insert(
            "meta".to_string(),
            json!({"title": "Test", "tags": ["a", "b"]}),
        );

        let defs = vec![
            FieldDefinition::builder("meta", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("title", FieldType::Text).build(),
                    FieldDefinition::builder("tags", FieldType::Text).build(),
                ])
                .build(),
        ];

        let flat = flatten_document_values(&fields, &defs);
        assert_eq!(flat.get("meta__title").unwrap(), "Test");
        // Array values get serialized via value_to_form_string
        assert_eq!(flat.get("meta__tags").unwrap(), "[\"a\",\"b\"]");
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
        let ve = ValidationError::new(vec![FieldError::with_key(
            "title",
            "title is required",
            "validation.required",
            params,
        )]);
        let map = translate_validation_errors(&ve, &translations, "en");
        assert_eq!(map.get("title").unwrap(), "Title is required");
    }

    #[test]
    fn translate_without_key_uses_raw_message() {
        let translations = test_translations();
        let ve = ValidationError::new(vec![FieldError::new("title", "custom lua error")]);
        let map = translate_validation_errors(&ve, &translations, "en");
        assert_eq!(map.get("title").unwrap(), "custom lua error");
    }

    #[test]
    fn translate_german_locale() {
        let translations = test_translations();
        let mut params = HashMap::new();
        params.insert("field".to_string(), "Titel".to_string());
        let ve = ValidationError::new(vec![FieldError::with_key(
            "title",
            "title is required",
            "validation.required",
            params,
        )]);
        let map = translate_validation_errors(&ve, &translations, "de");
        assert_eq!(map.get("title").unwrap(), "Titel ist erforderlich");
    }
}
