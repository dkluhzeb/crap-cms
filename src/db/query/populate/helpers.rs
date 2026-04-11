//! Helper functions for the populate subsystem.

use anyhow::Result;
use serde_json::{Map, Value};

use crate::core::{Document, cache::CacheBackend};

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

    use crate::core::Document;

    use super::*;

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
