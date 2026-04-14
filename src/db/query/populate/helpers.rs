//! Helper functions for the populate subsystem.

use anyhow::Result;
use serde_json::{Map, Value};

use crate::core::{Document, cache::CacheBackend};
use crate::db::query::populate::Singleflight;

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

/// Result of a cache-or-fetch attempt.
///
/// Callers can tell a cache hit (already fully populated) from a fresh fetch
/// (caller still needs to run recursive population and write the populated
/// version back to the cache).
pub(super) enum CacheOrFetch {
    /// Cache hit: returned document is already fully populated. Callers MUST
    /// use it as-is and skip the recursive populate step.
    Hit(Document),
    /// Fresh fetch via singleflight: returned `Some(doc)` is a raw (not yet
    /// recursively populated) document; `None` means the target is missing.
    Fresh(Option<Document>),
}

/// Get a populated document from the cache, or fetch it via the singleflight
/// (deduplicating concurrent misses for the same key).
///
/// On cache hit, returns `Hit(doc)` without consulting the singleflight.
/// On miss, the first thread runs `fetch` and writes the result into the
/// cache; concurrent misses for the same key wait for that fetch to
/// complete and receive the same value — collapsing N concurrent DB queries
/// into one. The resulting `Fresh(...)` value is the raw (pre-populate)
/// document; callers are still expected to recursively populate it and then
/// update the cache with the populated version.
///
/// `fetch` returns `Option<Document>` so a "not found" result also dedupes
/// (all concurrent waiters learn the miss without re-querying).
pub(super) fn cache_or_fetch_doc<F>(
    cache: &dyn CacheBackend,
    singleflight: &Singleflight<Option<Document>>,
    key: &str,
    fetch: F,
) -> CacheOrFetch
where
    F: FnOnce() -> Option<Document>,
{
    if let Ok(Some(doc)) = cache_get_doc(cache, key) {
        return CacheOrFetch::Hit(doc);
    }

    let fresh = singleflight.get_or_fetch(key, || {
        let doc = fetch();

        if let Some(ref d) = doc {
            let _ = cache_set_doc(cache, key, d);
        }

        doc
    });

    CacheOrFetch::Fresh(fresh)
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
    let mut map = Map::new();

    map.insert("id".to_string(), Value::String(doc.id.to_string()));
    map.insert(
        "collection".to_string(),
        Value::String(collection.to_string()),
    );

    for (k, v) in &doc.fields {
        map.insert(k.clone(), v.clone());
    }

    if let Some(ref ts) = doc.created_at {
        map.insert("created_at".to_string(), Value::String(ts.clone()));
    }

    if let Some(ref ts) = doc.updated_at {
        map.insert("updated_at".to_string(), Value::String(ts.clone()));
    }

    Value::Object(map)
}

#[cfg(test)]
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
        match result {
            CacheOrFetch::Hit(d) => assert_eq!(d.id.as_ref(), "d1"),
            _ => panic!("expected cache hit"),
        }
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
        match result {
            CacheOrFetch::Fresh(Some(d)) => assert_eq!(d.id.as_ref(), "d1"),
            _ => panic!("expected fresh Some"),
        }

        // Second call should hit the cache now (fetch closure must not run).
        let result2 = cache_or_fetch_doc(&cache, &sf, "k", || {
            counter.fetch_add(1, Ordering::SeqCst);
            None
        });

        assert_eq!(counter.load(Ordering::SeqCst), 1, "fetch should not re-run");
        assert!(matches!(result2, CacheOrFetch::Hit(_)));
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
        assert!(matches!(r, CacheOrFetch::Fresh(None)));
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

        let mut got_hit = 0;
        let mut got_fresh = 0;

        for h in handles {
            match h.join().unwrap() {
                CacheOrFetch::Hit(_) => got_hit += 1,
                CacheOrFetch::Fresh(Some(_)) => got_fresh += 1,
                CacheOrFetch::Fresh(None) => panic!("unexpected None"),
            }
        }

        // Exactly one thread ran the DB fetch.
        assert_eq!(
            counter.load(Ordering::SeqCst),
            1,
            "concurrent cache misses must collapse into a single fetch"
        );
        // All N threads got a result; some via Fresh (from singleflight),
        // possibly some via Hit (if scheduling let them observe the cache
        // write before their singleflight call). The important invariant is
        // the fetch-count above.
        assert_eq!(got_hit + got_fresh, n);
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
        assert_eq!(obj.get("count").and_then(|v| v.as_i64()), Some(42));
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
        assert_eq!(meta.get("score").and_then(|v| v.as_f64()), Some(9.5));
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
