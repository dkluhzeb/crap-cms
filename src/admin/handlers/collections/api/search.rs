//! Collection search handler for relationship field search.

use axum::{
    Extension, Json,
    extract::{Path, Query, State},
};
use serde_json::{Value, json};

use crate::{
    admin::{
        AdminState,
        context::CollectionPermissions,
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
    let default_limit = usize::try_from(state.config.pagination.default_limit.max(0)).unwrap_or(20);
    let max_limit = usize::try_from(state.config.pagination.max_limit.max(0)).unwrap_or(1000);
    let limit = params.limit.unwrap_or(default_limit).min(max_limit);

    let Ok(conn) = state.pool.get() else {
        return Json(json!([]));
    };

    let locale_ctx = LocaleContext::from_locale_string(None, &state.config.locale).unwrap_or(None);
    let user_doc = get_user_doc(auth_user.as_ref());

    let read_hooks = service::RunnerReadHooks::new(&state.hook_runner, &conn, user_doc, None);

    let search = if search_term.is_empty() {
        None
    } else {
        Some(search_term.clone())
    };

    // Overflow path falls back to a small page rather than i64::MAX —
    // an unbounded LIMIT would return the entire collection.
    let fq = FindQuery::builder()
        .limit(Some(i64::try_from(limit).unwrap_or(20)))
        .search(search)
        .build();

    let ctx = service::ServiceContext::collection(&slug, def)
        .pool(&state.pool)
        .conn(&conn)
        .read_hooks(&read_hooks)
        .user(user_doc)
        .build();

    // Editors picking a related document should see drafts too (work-in-progress
    // content), but only if they can actually view drafts — otherwise the read
    // path's edit-level draft gate would reject the whole search and return no
    // results at all. A read-only admin sees published candidates.
    let can_view_drafts = CollectionPermissions::for_user(&state, def, auth_user.as_ref()).draft;

    let search_input = service::SearchDocumentsInput {
        query: &fq,
        locale_ctx: locale_ctx.as_ref(),
        cursor_enabled: false,
        include_drafts: can_view_drafts,
    };

    let Ok(result) = service::search_documents(&ctx, &search_input) else {
        return Json(json!([]));
    };

    let title_field = def.title_field().map(std::string::ToString::to_string);
    let is_upload = def.upload.as_ref().is_some_and(|u| u.enabled);
    let admin_thumbnail = def.upload.as_ref().and_then(|u| u.admin_thumbnail.clone());

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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::Value;

    use crate::core::document::DocumentBuilder;

    use super::*;

    fn doc(fields: serde_json::Value) -> Document {
        let map: HashMap<String, Value> = serde_json::from_value(fields).unwrap();
        DocumentBuilder::new("id-1").fields(map).build()
    }

    #[test]
    fn upload_prefers_filename_then_title_then_id() {
        let with_file = doc(json!({ "filename": "pic.png", "name": "Pic" }));
        assert_eq!(doc_label(&with_file, Some("name"), true), "pic.png");

        let no_file = doc(json!({ "name": "Pic" }));
        assert_eq!(doc_label(&no_file, Some("name"), true), "Pic");

        let bare = doc(json!({}));
        assert_eq!(doc_label(&bare, Some("name"), true), "id-1");
    }

    #[test]
    fn non_upload_uses_title_field_then_id() {
        let d = doc(json!({ "title": "Hello" }));
        assert_eq!(doc_label(&d, Some("title"), false), "Hello");

        // title_field missing from doc, or no title_field → fall back to id.
        assert_eq!(doc_label(&d, Some("absent"), false), "id-1");
        assert_eq!(doc_label(&d, None, false), "id-1");
    }
}
