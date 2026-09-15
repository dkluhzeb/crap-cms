//! Collection search handler for relationship field search.

use axum::{
    Extension, Json,
    extract::{Path, Query, State},
    http::HeaderMap,
};
use serde_json::{Value, json};
use tracing::warn;

use crate::{
    admin::{
        AdminState,
        handlers::{
            collections::shared::thumbnail_url,
            shared::{
                editor_locale_ctx, extract_editor_locale, get_user_doc,
                response::on_blocking_section,
            },
        },
    },
    config::LocaleConfig,
    core::{Document, auth::AuthUser},
    db::{FindQuery, LocaleContext},
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

/// The locale search labels are read in: the editor's locale cookie, as the
/// list and edit views read — the default locale without a valid one.
fn search_locale_ctx(headers: &HeaderMap, config: &LocaleConfig) -> Option<LocaleContext> {
    editor_locale_ctx(config, extract_editor_locale(headers, config).as_deref())
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
    headers: HeaderMap,
    auth_user: Option<Extension<AuthUser>>,
) -> Json<Value> {
    let locale_ctx = search_locale_ctx(&headers, &state.config.locale);

    let Some(def) = state.infra.registry.get_collection(&slug).cloned() else {
        return Json(json!([]));
    };

    let search_term = params.q.unwrap_or_default().to_lowercase();
    // Clamp through the shared pagination chokepoint: floors to 1 (never
    // `LIMIT 0`) and caps at `max_limit`, matching `PaginationParams::resolve`.
    let requested = params.limit.map(|l| i64::try_from(l).unwrap_or(i64::MAX));
    let limit = state.config.pagination.resolve_limit(requested);

    // The DB checkout, the FTS/LIKE search, and its `after_read` /
    // field-read-strip Lua post-processing (VM acquire up to 5s) all run
    // synchronously — do them on the blocking pool, not the async worker
    //. This endpoint fires on every keystroke in a
    // relationship picker, so parking a runtime worker here is the worst
    // case for the whole admin. Runs inline under a current-thread
    // runtime (tests).
    on_blocking_section(move || {
        // Autocomplete endpoint: always answers with a JSON array. A missing
        // collection is silent (client/config issue), but a real DB failure is
        // logged so it isn't invisibly masked as "no results".
        let conn = match state.infra.pool.get() {
            Ok(conn) => conn,
            Err(e) => {
                warn!("Relationship search: DB pool unavailable for '{slug}': {e}");
                return Json(json!([]));
            }
        };

        let user_doc = get_user_doc(auth_user.as_ref());

        let read_hooks =
            service::RunnerReadHooks::new(&state.infra.hook_runner, &conn, user_doc, None);

        let search = if search_term.is_empty() {
            None
        } else {
            Some(search_term.clone())
        };

        let fq = FindQuery::builder()
            .limit(Some(limit))
            .search(search)
            .build();

        let ctx = service::ServiceContext::collection(&slug, &def)
            .pool(&state.infra.pool)
            .conn(&conn)
            .read_hooks(&read_hooks)
            .user(user_doc)
            .build();

        // Request drafts unconditionally — the service search downgrades (never
        // rejects): an editor sees work-in-progress draft candidates, a read-only
        // admin sees published candidates only. The draft gate is the service's job.
        let search_input = service::SearchDocumentsInput {
            query: &fq,
            locale_ctx: locale_ctx.as_ref(),
            cursor_enabled: false,
            include_drafts: true,
        };

        let result = match service::search_documents(&ctx, &search_input) {
            Ok(result) => result,
            Err(e) => {
                warn!("Relationship search failed for '{slug}': {e}");
                return Json(json!([]));
            }
        };

        let title_field = def.title_field().map(std::string::ToString::to_string);
        let is_upload = def.is_upload_collection();
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
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use axum::http::header;
    use serde_json::from_value;

    use crate::core::document::DocumentBuilder;

    use super::*;

    fn doc(fields: Value) -> Document {
        let map: HashMap<String, Value> = from_value(fields).unwrap();
        DocumentBuilder::new("id-1").fields(map).build()
    }

    fn locale_config() -> LocaleConfig {
        LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: false,
        }
    }

    fn cookie_headers(cookie: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, cookie.parse().unwrap());
        headers
    }

    /// Search labels are read in the locale of the editor's locale cookie.
    #[test]
    fn search_reads_labels_in_the_editor_locale() {
        let headers = cookie_headers("crap_session=abc; crap_editor_locale=de");

        let ctx = search_locale_ctx(&headers, &locale_config()).expect("a context");

        assert_eq!(ctx.access_locale(), "de");
    }

    /// Without a cookie, or with one naming an unconfigured locale, labels are
    /// read in the default locale.
    #[test]
    fn search_reads_labels_in_the_default_locale_without_a_valid_cookie() {
        let config = locale_config();

        for headers in [HeaderMap::new(), cookie_headers("crap_editor_locale=zz")] {
            let ctx = search_locale_ctx(&headers, &config).expect("a context");

            assert_eq!(ctx.access_locale(), "en");
        }
    }

    #[test]
    fn search_reads_without_a_locale_when_localization_is_off() {
        let headers = cookie_headers("crap_editor_locale=de");

        assert!(search_locale_ctx(&headers, &LocaleConfig::default()).is_none());
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
