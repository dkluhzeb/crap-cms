//! Collection search handler for relationship field search.

use crate::core::auth::{AuthUser, Claims};
use crate::core::upload;
use crate::db::query;
use crate::db::query::{AccessResult, FindQuery};
use axum::{
    extract::{Path, State},
    Extension,
};

use crate::admin::handlers::shared::check_access_or_forbid;
use crate::admin::AdminState;

/// Search query parameters for collection search.
#[derive(serde::Deserialize)]
pub struct SearchQuery {
    /// The search term to filter results by.
    pub q: Option<String>,
    /// The maximum number of results to return.
    pub limit: Option<usize>,
}

/// GET /admin/api/search/{collection}?q=...&limit=20
/// Returns JSON array of `{id, label}` for relationship field search.
pub async fn search_collection(
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    axum::extract::Query(params): axum::extract::Query<SearchQuery>,
    _claims: Option<Extension<Claims>>,
    auth_user: Option<Extension<AuthUser>>,
) -> axum::Json<serde_json::Value> {
    let Some(def) = state.registry.get_collection(&slug) else {
        return axum::Json(serde_json::json!([]));
    };

    // Check read access
    let access_result =
        match check_access_or_forbid(&state, def.access.read.as_deref(), &auth_user, None, None) {
            Ok(r) => r,
            Err(_) => return axum::Json(serde_json::json!([])),
        };
    if matches!(access_result, AccessResult::Denied) {
        return axum::Json(serde_json::json!([]));
    }

    let title_field = def.title_field().map(|s| s.to_string());
    let search_term = params.q.unwrap_or_default().to_lowercase();
    let limit = params
        .limit
        .unwrap_or(state.config.pagination.default_limit as usize)
        .min(state.config.pagination.max_limit as usize);

    let is_upload = def.upload.as_ref().is_some_and(|u| u.enabled);
    let admin_thumbnail = def
        .upload
        .as_ref()
        .and_then(|u| u.admin_thumbnail.as_ref().cloned());

    let Ok(conn) = state.pool.get() else {
        return axum::Json(serde_json::json!([]));
    };

    let locale_ctx =
        crate::db::query::LocaleContext::from_locale_string(None, &state.config.locale);
    let mut find_query = FindQuery::new();
    find_query.limit = Some(limit as i64);
    find_query.search = if search_term.is_empty() {
        None
    } else {
        Some(search_term.clone())
    };

    // Merge access constraint filters
    if let AccessResult::Constrained(ref constraint_filters) = access_result {
        find_query.filters.extend(constraint_filters.clone());
    }

    let Ok(mut docs) = query::find(&conn, &slug, def, &find_query, locale_ctx.as_ref()) else {
        return axum::Json(serde_json::json!([]));
    };

    // Assemble sizes for upload collections so we can extract thumbnail URLs
    if is_upload {
        if let Some(ref upload_config) = def.upload {
            for doc in &mut docs {
                upload::assemble_sizes_object(doc, upload_config);
            }
        }
    }

    let results: Vec<_> = docs
        .iter()
        .map(|doc| {
            let label = if is_upload {
                doc.get_str("filename")
                    .or_else(|| title_field.as_ref().and_then(|f| doc.get_str(f)))
                    .unwrap_or(&doc.id)
                    .to_string()
            } else {
                title_field
                    .as_ref()
                    .and_then(|f| doc.get_str(f))
                    .unwrap_or(&doc.id)
                    .to_string()
            };
            let mut item = serde_json::json!({ "id": doc.id, "label": label });
            if is_upload {
                let mime = doc.get_str("mime_type").unwrap_or("");
                let is_image = mime.starts_with("image/");
                let thumb_url = if is_image {
                    admin_thumbnail
                        .as_ref()
                        .and_then(|thumb_name| {
                            doc.fields
                                .get("sizes")
                                .and_then(|v| v.get(thumb_name))
                                .and_then(|v| v.get("url"))
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string())
                        })
                        .or_else(|| doc.get_str("url").map(|s| s.to_string()))
                } else {
                    None
                };
                if let Some(url) = thumb_url {
                    item["thumbnail_url"] = serde_json::json!(url);
                }
                item["filename"] = serde_json::json!(label);
                if is_image {
                    item["is_image"] = serde_json::json!(true);
                }
            }
            item
        })
        .collect();

    axum::Json(serde_json::json!(results))
}
