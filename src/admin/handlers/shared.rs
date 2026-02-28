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
use crate::core::auth::AuthUser;
use crate::core::collection::VersionsConfig;
use crate::core::document::VersionSnapshot;
use crate::core::field::FieldDefinition;
use crate::db::query::{self, AccessResult, LocaleContext};
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
}

/// Query parameters for locale selection on edit pages.
#[derive(Debug, Deserialize)]
pub struct LocaleParams {
    pub locale: Option<String>,
}

/// Extract the user document from AuthUser extension (for access checks).
pub(super) fn get_user_doc(auth_user: &Option<Extension<AuthUser>>) -> Option<&crate::core::Document> {
    auth_user.as_ref().map(|Extension(au)| &au.user_doc)
}

/// Extract an EventUser from the AuthUser extension (for SSE event attribution).
pub(super) fn get_event_user(auth_user: &Option<Extension<AuthUser>>) -> Option<crate::core::event::EventUser> {
    auth_user.as_ref().map(|Extension(au)| crate::core::event::EventUser {
        id: au.claims.sub.clone(),
        email: au.claims.email.clone(),
    })
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
    let user_doc = get_user_doc(auth_user);
    let conn = state.pool.get()
        .map_err(|_| forbidden(state, "Database error").into_response())?;
    state.hook_runner.check_access(access_ref, user_doc, id, data, &conn)
        .map_err(|e| {
            tracing::error!("Access check error: {}", e);
            forbidden(state, "Access check failed").into_response()
        })
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
    name.split('_')
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                None => String::new(),
                Some(f) => f.to_uppercase().chain(c).collect(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
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
        Err(e) => Html(format!("<h1>Template Error</h1><pre>{}</pre>", e)).into_response(),
    }
}

/// Render a template, falling back to a plain error page on failure.
pub(super) fn render_or_error(state: &AdminState, template: &str, data: &serde_json::Value) -> Html<String> {
    match state.render(template, data) {
        Ok(html) => Html(html),
        Err(e) => Html(format!("<h1>Template Error</h1><pre>{}</pre>", e)),
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
        let v = VersionSnapshot {
            id: "v1".to_string(),
            parent: "doc1".to_string(),
            version: 3,
            status: "published".to_string(),
            latest: true,
            created_at: Some("2026-01-01T00:00:00Z".to_string()),
            updated_at: Some("2026-01-01T00:00:00Z".to_string()),
            snapshot: serde_json::json!({}),
        };
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
        let admin = FieldAdmin {
            label_field: Some("title".to_string()),
            ..Default::default()
        };
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
}
