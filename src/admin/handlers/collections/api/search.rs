//! Collection search handler for relationship field search.

use axum::{
    Extension, Json,
    extract::{Path, Query, State},
};
use serde_json::{Value, json};

use crate::{
    admin::{
        AdminState,
        handlers::{collections::shared::thumbnail_url, shared::check_access_or_forbid},
    },
    core::{CollectionDefinition, Document, auth::AuthUser, upload},
    db::{
        BoxedConnection,
        query::{self, AccessResult, FindQuery, LocaleContext},
    },
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

/// Execute the search query and assemble upload sizes if applicable.
fn fetch_search_results(
    conn: &BoxedConnection,
    slug: &str,
    def: &CollectionDefinition,
    search_term: &str,
    limit: usize,
    access_result: &AccessResult,
    locale_ctx: Option<&LocaleContext>,
) -> Option<Vec<Document>> {
    let mut find_query = FindQuery::new();
    find_query.limit = Some(limit as i64);

    find_query.search = if search_term.is_empty() {
        None
    } else {
        Some(search_term.to_string())
    };

    if let AccessResult::Constrained(constraint_filters) = access_result {
        find_query.filters.extend(constraint_filters.clone());
    }

    let mut docs = query::find(conn, slug, def, &find_query, locale_ctx).ok()?;

    if def.upload.as_ref().is_some_and(|u| u.enabled)
        && let Some(upload_config) = &def.upload
    {
        for doc in &mut docs {
            upload::assemble_sizes_object(doc, upload_config);
        }
    }

    Some(docs)
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

    let access_result =
        match check_access_or_forbid(&state, def.access.read.as_deref(), &auth_user, None, None) {
            Ok(r) => r,
            Err(_) => return Json(json!([])),
        };

    if matches!(access_result, AccessResult::Denied) {
        return Json(json!([]));
    }

    let search_term = params.q.unwrap_or_default().to_lowercase();
    let limit = params
        .limit
        .unwrap_or(state.config.pagination.default_limit as usize)
        .min(state.config.pagination.max_limit as usize);

    let Ok(conn) = state.pool.get() else {
        return Json(json!([]));
    };

    let locale_ctx = LocaleContext::from_locale_string(None, &state.config.locale);

    let docs = match fetch_search_results(
        &conn,
        &slug,
        def,
        &search_term,
        limit,
        &access_result,
        locale_ctx.as_ref(),
    ) {
        Some(d) => d,
        None => return Json(json!([])),
    };

    let title_field = def.title_field().map(|s| s.to_string());
    let is_upload = def.upload.as_ref().is_some_and(|u| u.enabled);
    let admin_thumbnail = def
        .upload
        .as_ref()
        .and_then(|u| u.admin_thumbnail.as_ref().cloned());

    let results: Vec<_> = docs
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
