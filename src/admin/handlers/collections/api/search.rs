//! Collection search handler for relationship field search.

use axum::{
    Extension, Json,
    extract::{Path, Query, State},
};
use serde_json::{Value, json};

use crate::{
    admin::{
        AdminState,
        handlers::{collections::shared::thumbnail_url, shared::get_user_doc},
    },
    core::{Document, auth::AuthUser},
    db::{FindQuery, query::LocaleContext},
    service,
};

/// Search query parameters for collection search.
#[derive(serde::Deserialize)]
pub struct SearchQuery {
    /// The search term to filter results by.
    pub q: Option<String>,
    /// The maximum number of results to return.
    pub limit: Option<usize>,
}

/// Extract the display label for a document (upload filename or title field).
fn doc_label(doc: &Document, title_field: Option<&str>, is_upload: bool) -> String {
    if is_upload {
        doc.get_str("filename")
            .or_else(|| title_field.and_then(|f| doc.get_str(f)))
            .unwrap_or(&doc.id)
            .to_string()
    } else {
        title_field
            .and_then(|f| doc.get_str(f))
            .unwrap_or(&doc.id)
            .to_string()
    }
}

/// Build a search result JSON object for a single document.
fn build_search_result(
    doc: &Document,
    title_field: Option<&str>,
    is_upload: bool,
    admin_thumbnail: Option<&str>,
) -> Value {
    let label = doc_label(doc, title_field, is_upload);
    let mut item = json!({ "id": doc.id, "label": label });

    if is_upload {
        if let Some(url) = thumbnail_url(doc, admin_thumbnail) {
            item["thumbnail_url"] = json!(url);
        }

        item["filename"] = json!(label);

        let is_image = doc.get_str("mime_type").unwrap_or("").starts_with("image/");

        if is_image {
            item["is_image"] = json!(true);
        }
    }

    item
}

/// GET /admin/api/search/{collection}?q=...&limit=20
/// Returns JSON array of `{id, label}` for relationship field search.
pub async fn search_collection(
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    Query(params): Query<SearchQuery>,
    auth_user: Option<Extension<AuthUser>>,
) -> Json<Value> {
    let Some(def) = state.registry.get_collection(&slug) else {
        return Json(json!([]));
    };

    let search_term = params.q.unwrap_or_default().to_lowercase();
    let limit = params
        .limit
        .unwrap_or(state.config.pagination.default_limit as usize)
        .min(state.config.pagination.max_limit as usize);

    let Ok(conn) = state.pool.get() else {
        return Json(json!([]));
    };

    let locale_ctx = LocaleContext::from_locale_string(None, &state.config.locale).unwrap_or(None);
    let user_doc = get_user_doc(&auth_user);

    let read_hooks = service::RunnerReadHooks::new(&state.hook_runner, &conn);

    let search = if search_term.is_empty() {
        None
    } else {
        Some(search_term.to_string())
    };

    let mut fq = FindQuery::new();
    fq.limit = Some(limit as i64);
    fq.search = search;

    let ctx = service::ServiceContext::collection(&slug, def)
        .pool(&state.pool)
        .conn(&conn)
        .read_hooks(&read_hooks)
        .user(user_doc)
        .build();

    let search_input = service::SearchDocumentsInput {
        query: &fq,
        locale_ctx: locale_ctx.as_ref(),
        cursor_enabled: false,
    };

    let result = match service::search_documents(&ctx, &search_input) {
        Ok(r) => r,
        Err(_) => return Json(json!([])),
    };

    let title_field = def.title_field().map(|s| s.to_string());
    let is_upload = def.upload.as_ref().is_some_and(|u| u.enabled);
    let admin_thumbnail = def
        .upload
        .as_ref()
        .and_then(|u| u.admin_thumbnail.as_ref().cloned());

    let results: Vec<_> = result
        .docs
        .iter()
        .map(|doc| {
            build_search_result(
                doc,
                title_field.as_deref(),
                is_upload,
                admin_thumbnail.as_deref(),
            )
        })
        .collect();

    Json(json!(results))
}
