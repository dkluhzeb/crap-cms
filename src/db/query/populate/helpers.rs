//! Helper functions for the populate subsystem.

use anyhow::Result;
use serde::Serialize;
use serde_json::Value;

use crate::core::{
    CollectionDefinition, Document, DocumentFields, FieldDefinition, cache::CacheBackend,
};
use crate::db::query::AccessResult;
use crate::db::query::filter::memory::matches_constraints_typed;
use crate::db::query::populate::JoinAccessCheck;
use crate::db::query::populate::PopulateCtx;
use crate::db::query::populate::Singleflight;
use crate::db::query::read::{find_by_id, find_by_ids};

/// Fetch a single **raw** relationship target by id. The result is the
/// unfiltered document content — draft visibility and access are applied per
/// request by the caller, so the raw doc can be shared in the cross-user
/// populate cache. See [`resolve_target_views`] / [`target_row_visible`].
pub(super) fn fetch_target(
    ctx: &PopulateCtx<'_>,
    slug: &str,
    def: &CollectionDefinition,
    id: &str,
) -> Result<Option<Document>> {
    find_by_id(ctx.conn, slug, def, id, ctx.locale_ctx)
}

/// Batch variant of [`fetch_target`] — returns raw documents; the caller applies
/// the per-request draft and access filters.
pub(super) fn fetch_targets(
    ctx: &PopulateCtx<'_>,
    slug: &str,
    def: &CollectionDefinition,
    ids: &[String],
) -> Result<Vec<Document>> {
    find_by_ids(ctx.conn, slug, def, ids, ctx.locale_ctx)
}

/// A relationship target collection's resolved view access — the `read` decision
/// (gates **published** target rows) and the `draft` decision (gates **draft**
/// target rows, `access.draft ?? access.update`). Both are collection-level (not
/// per-row), so the caller resolves them once per field and applies them to every
/// target. See [`target_row_visible`].
pub(super) struct TargetViews {
    pub read: AccessResult,
    pub draft: AccessResult,
}

impl TargetViews {
    /// Both views allowed — the no-access-wired default (legacy/internal callers).
    pub(super) fn allow_all() -> Self {
        Self {
            read: AccessResult::Allowed,
            draft: AccessResult::Allowed,
        }
    }
}

/// Resolve a target collection's view access once for a field-population call.
///
/// Returns `Allowed` for both views when no access check is wired
/// (legacy/internal callers). The `draft` decision is only consulted for
/// draft-enabled targets; it is resolved unconditionally (cheap) so the router
/// can stay a pure function.
pub(super) fn resolve_target_views(
    ctx: &PopulateCtx<'_>,
    slug: &str,
    def: &CollectionDefinition,
) -> Result<TargetViews> {
    resolve_views(ctx.join_access, ctx.user, slug, def)
}

/// Resolve a target collection's view access from a raw `(join_access, user)`
/// pair — shared by the relationship path ([`resolve_target_views`], via
/// `PopulateCtx`) and the join path (which carries `PopulateOpts`). `read` gates
/// published rows; `draft` (`access.draft ?? access.update`) gates draft rows.
pub(super) fn resolve_views(
    join_access: Option<&dyn JoinAccessCheck>,
    user: Option<&Document>,
    slug: &str,
    def: &CollectionDefinition,
) -> Result<TargetViews> {
    let Some(check) = join_access else {
        return Ok(TargetViews::allow_all());
    };

    let read = check.check(def.access.read.as_ref(), user, slug)?;
    let draft = check.check(def.access.resolve_draft(), user, slug)?;

    Ok(TargetViews { read, draft })
}

/// Whether a raw target document is visible to the viewer, applying the full
/// independent-views model in memory (the cache holds raw, user-independent
/// docs, so visibility is decided per request, per row).
///
/// - A **published** row needs the target's `read` view.
/// - A **draft** row needs drafts to be *requested* (`!published_only`, inherited
///   from the parent read's draft opt-in) **and** the target's `draft` view —
///   mirroring the top-level read union (requested × allowed). A published-only
///   read never embeds drafts; a draft-opted read embeds a draft only where the
///   target grants draft access.
/// - A non-draft target collection (no `_status` column) is gated by `read` only.
///
/// `Constrained` filters are matched against the **raw** fields (before any
/// population turns relationship fields into objects).
pub(super) fn target_row_visible(
    views: &TargetViews,
    raw: &Document,
    published_only: bool,
    def: &CollectionDefinition,
) -> bool {
    if !def.has_drafts() {
        return view_allows(&views.read, raw, &def.fields);
    }

    let is_draft = raw.fields.get("_status").and_then(|v| v.as_str()) != Some("published");
    if is_draft {
        return !published_only && view_allows(&views.draft, raw, &def.fields);
    }

    view_allows(&views.read, raw, &def.fields)
}

/// Whether a raw target document passes one resolved view decision. `fields` is
/// the target collection's schema, so a `Constrained` filter on a Checkbox /
/// Number field is coerced the same way SQL binds it (not a blind string match).
fn view_allows(access: &AccessResult, raw: &Document, fields: &[FieldDefinition]) -> bool {
    match access {
        AccessResult::Denied => false,
        AccessResult::Allowed => true,
        AccessResult::Constrained(filters) => {
            matches_constraints_typed(&raw.fields, filters, fields)
        }
    }
}

/// The shape `document_to_json` emits — a populated relationship reference
/// embedded in a parent document's `fields`. `id` and `collection` are the
/// envelope; the document's user-defined fields flatten alongside them.
///
/// Wire-format equivalent to the previous manual `Map::new() + insert` loop —
/// `#[serde(flatten)]` over `DocumentFields` (transparent over `HashMap`)
/// reproduces the same key set in the same order.
#[derive(Serialize)]
struct PopulatedRef<'a> {
    id: &'a str,
    collection: &'a str,
    #[serde(flatten)]
    fields: &'a DocumentFields,
    #[serde(skip_serializing_if = "Option::is_none")]
    created_at: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    updated_at: Option<&'a str>,
}

/// Try to get a cached document from the cache backend.
pub(super) fn cache_get_doc(cache: &dyn CacheBackend, key: &str) -> Result<Option<Document>> {
    match cache.get(key)? {
        Some(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        None => Ok(None),
    }
}

/// Store a document in the cache backend.
pub(super) fn cache_set_doc(cache: &dyn CacheBackend, key: &str, doc: &Document) -> Result<()> {
    let bytes = serde_json::to_vec(doc)?;

    cache.set(key, &bytes)?;

    Ok(())
}

/// Fetch a **raw** target document by key: cache first, else the singleflight
/// runs `fetch` once (deduplicating concurrent misses for the same key) and
/// caches the raw result. Returns the raw doc, or `None` if missing.
///
/// The cache only ever holds raw, user-independent content; population, draft
/// filtering, and `read` access filtering are applied per request by the caller
/// on every retrieval (hit and miss). `fetch` returns `Option<Document>` so a
/// "not found" result also dedupes (all concurrent waiters learn the miss).
pub(super) fn cache_or_fetch_doc<F>(
    cache: &dyn CacheBackend,
    singleflight: &Singleflight<Option<Document>>,
    key: &str,
    fetch: F,
) -> Option<Document>
where
    F: FnOnce() -> Option<Document>,
{
    if let Ok(Some(doc)) = cache_get_doc(cache, key) {
        return Some(doc);
    }

    singleflight.get_or_fetch(key, || {
        let doc = fetch();

        if let Some(ref d) = doc {
            let _ = cache_set_doc(cache, key, d);
        }

        doc
    })
}

/// Parse a polymorphic reference "collection/id" into `(collection, id)`.
pub(crate) fn parse_poly_ref(s: &str) -> Option<(String, String)> {
    let (col, id) = s.split_once('/')?;

    if col.is_empty() || id.is_empty() {
        return None;
    }

    Some((col.to_string(), id.to_string()))
}

/// Convert a Document into a JSON Value for embedding in a parent's fields.
pub(crate) fn document_to_json(doc: &Document, collection: &str) -> Value {
    let id = doc.id.as_ref();
    let populated = PopulatedRef {
        id,
        collection,
        fields: &doc.fields,
        created_at: doc.created_at.as_deref(),
        updated_at: doc.updated_at.as_deref(),
    };

    serde_json::to_value(&populated).expect("PopulatedRef serializes")
}

#[cfg(test)]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::case_sensitive_file_extension_comparisons,
    clippy::items_after_statements,
    clippy::match_wildcard_for_single_variants,
    clippy::missing_panics_doc,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::unreadable_literal,
    clippy::used_underscore_binding
)]
mod tests {
    use serde_json::json;
    use std::sync::{
        Arc, Barrier,
        atomic::{AtomicUsize, Ordering},
    };
    use std::thread;

    use crate::core::Document;
    use crate::core::cache::MemoryCache;

    use super::*;

    // ── cache_or_fetch_doc tests ──────────────────────────────────────────────

    #[test]
    fn cache_or_fetch_doc_hit_skips_fetch() {
        let cache = MemoryCache::new(10_000);
        let sf: Singleflight<Option<Document>> = Singleflight::new();

        // Pre-populate cache.
        let mut cached = Document::new("d1".to_string());
        cached.fields.insert("t".to_string(), json!("cached"));
        cache_set_doc(&cache, "k", &cached).unwrap();

        let counter = AtomicUsize::new(0);
        let result = cache_or_fetch_doc(&cache, &sf, "k", || {
            counter.fetch_add(1, Ordering::SeqCst);
            None
        });

        assert_eq!(counter.load(Ordering::SeqCst), 0);
        assert_eq!(result.expect("cache hit").id.as_ref(), "d1");
    }

    #[test]
    fn cache_or_fetch_doc_miss_runs_fetch_and_caches() {
        let cache = MemoryCache::new(10_000);
        let sf: Singleflight<Option<Document>> = Singleflight::new();
        let counter = AtomicUsize::new(0);

        let result = cache_or_fetch_doc(&cache, &sf, "k", || {
            counter.fetch_add(1, Ordering::SeqCst);
            let mut d = Document::new("d1".to_string());
            d.fields.insert("t".to_string(), json!("fresh"));
            Some(d)
        });

        assert_eq!(counter.load(Ordering::SeqCst), 1);
        assert_eq!(result.expect("fresh fetch").id.as_ref(), "d1");

        // Second call should hit the cache now (fetch closure must not run).
        let result2 = cache_or_fetch_doc(&cache, &sf, "k", || {
            counter.fetch_add(1, Ordering::SeqCst);
            None
        });

        assert_eq!(counter.load(Ordering::SeqCst), 1, "fetch should not re-run");
        assert_eq!(result2.expect("cache hit").id.as_ref(), "d1");
    }

    #[test]
    fn cache_or_fetch_doc_miss_none_dedupes_not_found() {
        let cache = MemoryCache::new(10_000);
        let sf: Singleflight<Option<Document>> = Singleflight::new();
        let counter = AtomicUsize::new(0);

        let r = cache_or_fetch_doc(&cache, &sf, "missing", || {
            counter.fetch_add(1, Ordering::SeqCst);
            None
        });
        assert!(r.is_none());
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    /// Regression: N concurrent cache-miss fetches for the same key must
    /// collapse into exactly one DB fetch.
    #[test]
    fn populate_deduplicates_concurrent_cache_miss() {
        let cache: Arc<MemoryCache> = Arc::new(MemoryCache::new(10_000));
        let sf: Arc<Singleflight<Option<Document>>> = Arc::new(Singleflight::new());

        let counter = Arc::new(AtomicUsize::new(0));
        let n = 16;
        let barrier = Arc::new(Barrier::new(n));

        let mut handles = Vec::new();

        for _ in 0..n {
            let cache = Arc::clone(&cache);
            let sf = Arc::clone(&sf);
            let counter = Arc::clone(&counter);
            let barrier = Arc::clone(&barrier);

            handles.push(thread::spawn(move || {
                barrier.wait();

                cache_or_fetch_doc(&*cache, &sf, "hot-key", || {
                    counter.fetch_add(1, Ordering::SeqCst);
                    thread::sleep(std::time::Duration::from_millis(40));

                    let mut d = Document::new("shared".to_string());
                    d.fields.insert("v".to_string(), json!("one"));
                    Some(d)
                })
            }));
        }

        let mut got = 0;
        for h in handles {
            assert!(h.join().unwrap().is_some(), "every caller gets the raw doc");
            got += 1;
        }

        // Exactly one thread ran the DB fetch; all N got the shared raw doc.
        assert_eq!(
            counter.load(Ordering::SeqCst),
            1,
            "concurrent cache misses must collapse into a single fetch"
        );
        assert_eq!(got, n);
    }

    // ── document_to_json tests ────────────────────────────────────────────────

    #[test]
    fn document_to_json_basic() {
        let mut doc = Document::new("doc1".to_string());
        doc.fields.insert("title".to_string(), json!("Hello World"));
        doc.fields.insert("count".to_string(), json!(42));
        doc.created_at = Some("2024-01-01T00:00:00Z".to_string());
        doc.updated_at = Some("2024-01-02T00:00:00Z".to_string());

        let json = document_to_json(&doc, "posts");
        let obj = json.as_object().expect("should be an object");

        assert_eq!(obj.get("id").and_then(|v| v.as_str()), Some("doc1"));
        assert_eq!(
            obj.get("collection").and_then(|v| v.as_str()),
            Some("posts")
        );
        assert_eq!(
            obj.get("title").and_then(|v| v.as_str()),
            Some("Hello World")
        );
        assert_eq!(
            obj.get("count").and_then(serde_json::Value::as_i64),
            Some(42)
        );
        assert_eq!(
            obj.get("created_at").and_then(|v| v.as_str()),
            Some("2024-01-01T00:00:00Z")
        );
        assert_eq!(
            obj.get("updated_at").and_then(|v| v.as_str()),
            Some("2024-01-02T00:00:00Z")
        );
    }

    #[test]
    fn document_to_json_no_timestamps() {
        let mut doc = Document::new("doc2".to_string());
        doc.fields
            .insert("title".to_string(), json!("No Timestamps"));
        // created_at and updated_at are None by default

        let json = document_to_json(&doc, "pages");
        let obj = json.as_object().expect("should be an object");

        assert_eq!(obj.get("id").and_then(|v| v.as_str()), Some("doc2"));
        assert_eq!(
            obj.get("collection").and_then(|v| v.as_str()),
            Some("pages")
        );
        assert_eq!(
            obj.get("title").and_then(|v| v.as_str()),
            Some("No Timestamps")
        );
        assert!(
            obj.get("created_at").is_none(),
            "created_at should be absent"
        );
        assert!(
            obj.get("updated_at").is_none(),
            "updated_at should be absent"
        );
    }

    #[test]
    fn document_to_json_with_nested() {
        let mut doc = Document::new("doc3".to_string());
        let nested = json!({
            "meta": {
                "keywords": ["rust", "cms"],
                "score": 9.5
            }
        });
        doc.fields.insert("data".to_string(), nested.clone());

        let json = document_to_json(&doc, "entries");
        let obj = json.as_object().expect("should be an object");

        assert_eq!(obj.get("data"), Some(&nested));
        // Verify deep structure is preserved
        let data = obj.get("data").unwrap();
        let meta = data.get("meta").expect("meta should exist");
        assert_eq!(
            meta.get("score").and_then(serde_json::Value::as_f64),
            Some(9.5)
        );
        let keywords = meta
            .get("keywords")
            .and_then(|v| v.as_array())
            .expect("keywords should be array");
        assert_eq!(keywords.len(), 2);
        assert_eq!(keywords[0].as_str(), Some("rust"));
    }

    // ── parse_poly_ref tests ──────────────────────────────────────────────────

    #[test]
    fn parse_poly_ref_valid() {
        assert_eq!(
            parse_poly_ref("articles/a1"),
            Some(("articles".to_string(), "a1".to_string()))
        );
        assert_eq!(
            parse_poly_ref("a/b"),
            Some(("a".to_string(), "b".to_string()))
        );
    }

    #[test]
    fn parse_poly_ref_no_slash_returns_none() {
        assert_eq!(parse_poly_ref("noslash"), None);
        assert_eq!(parse_poly_ref(""), None);
    }

    #[test]
    fn parse_poly_ref_empty_col_returns_none() {
        // "/id" — collection portion is empty
        assert_eq!(parse_poly_ref("/someid"), None);
    }

    #[test]
    fn parse_poly_ref_empty_id_returns_none() {
        // "col/" — id portion is empty
        assert_eq!(parse_poly_ref("col/"), None);
    }

    /// Regression: multi-byte UTF-8 in collection or id must not panic from string slicing.
    #[test]
    fn parse_poly_ref_multibyte_utf8() {
        assert_eq!(
            parse_poly_ref("記事/id1"),
            Some(("記事".to_string(), "id1".to_string()))
        );
        assert_eq!(
            parse_poly_ref("posts/日本語id"),
            Some(("posts".to_string(), "日本語id".to_string()))
        );
    }
}
