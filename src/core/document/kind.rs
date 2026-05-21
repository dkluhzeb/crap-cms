use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::{DocumentFields, DocumentId};
use crate::typegen::lua::LuaAnnotation;

/// A single content document with an ID, user-defined fields, and optional timestamps.
///
/// **Type-emission note:** the `LuaLS` class deliberately does NOT carry an
/// `[string] any` index signature. Adding one caused `LuaLS` to collapse
/// typed subclasses (`crap.doc.Posts : crap.Document`, etc.) back into
/// the permissive base — hover popups showed `crap.Document` even when
/// the per-collection narrowing was correct, because an index signature
/// of `any` subsumes the structural extension. Subclasses now expose
/// their typed fields cleanly; for the dynamic-slug `find()` fallback
/// (returning the bare `crap.FindResult`), users either cast with
/// `--[[@as crap.doc.X]]` or index via `doc["field"]` if they need to
/// reach a field outside the base shape.
#[derive(Debug, Clone, Serialize, Deserialize, LuaAnnotation)]
#[lua(class = "crap.Document")]
pub struct Document {
    /// Unique document ID (nanoid).
    #[lua(ty = "string")]
    pub id: DocumentId,
    /// User-defined field values, keyed by field name. Emitted as the
    /// flat top-level keys on the document table (see `extra_field`).
    #[serde(flatten)]
    #[lua(skip)]
    pub fields: DocumentFields,
    /// ISO 8601 timestamp (if `timestamps` enabled on the collection).
    #[serde(default)]
    #[lua(optional)]
    pub created_at: Option<String>,
    /// ISO 8601 timestamp (if `timestamps` enabled on the collection).
    #[serde(default)]
    #[lua(optional)]
    pub updated_at: Option<String>,
}

impl Document {
    /// Create an empty document with the given ID and no fields or timestamps.
    pub fn new(id: impl Into<DocumentId>) -> Self {
        Self {
            id: id.into(),
            fields: DocumentFields::new(),
            created_at: None,
            updated_at: None,
        }
    }

    /// Returns a new `DocumentBuilder` for constructing a document with the given ID.
    pub fn builder(id: impl Into<DocumentId>) -> DocumentBuilder {
        DocumentBuilder::new(id)
    }

    /// Get a field value by name.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.fields.get(key)
    }

    /// Get a field value as a string.
    #[must_use]
    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.fields.get_str(key)
    }

    /// Strip denied fields by name, handling both flat keys and `__`-separated
    /// group subfields. After hydration, `address__city` becomes nested
    /// `{"address": {"city": ...}}` — this method strips from both forms.
    pub fn strip_fields(&mut self, names: &[String]) {
        for name in names {
            // Flat removal (pre-hydration top-level keys like "secret" or "address__city")
            self.fields.remove(name);

            // Nested removal (post-hydration group subfields)
            let segments: Vec<&str> = name.split("__").collect();
            if segments.len() >= 2 {
                strip_nested(&mut self.fields, &segments);
            }
        }
    }
}

/// Walk into nested objects following `__`-separated segments and remove the leaf.
fn strip_nested(fields: &mut DocumentFields, segments: &[&str]) {
    let Some((&first, rest)) = segments.split_first() else {
        return;
    };

    let Some(Value::Object(map)) = fields.get_mut(first) else {
        return;
    };

    if rest.len() == 1 {
        map.remove(rest[0]);
    } else {
        strip_nested_value(map, rest);
    }
}

/// Recurse into a `serde_json::Map` to remove a deeply nested field.
fn strip_nested_value(map: &mut serde_json::Map<String, Value>, segments: &[&str]) {
    let Some((&first, rest)) = segments.split_first() else {
        return;
    };

    if rest.len() == 1 {
        if let Some(Value::Object(inner)) = map.get_mut(first) {
            inner.remove(rest[0]);
        }
    } else if let Some(Value::Object(inner)) = map.get_mut(first) {
        strip_nested_value(inner, rest);
    }
}

/// Builder for [`Document`].
pub struct DocumentBuilder {
    id: DocumentId,
    fields: DocumentFields,
    created_at: Option<String>,
    updated_at: Option<String>,
}

impl DocumentBuilder {
    /// Creates a new `DocumentBuilder` with the specified ID.
    pub fn new(id: impl Into<DocumentId>) -> Self {
        Self {
            id: id.into(),
            fields: DocumentFields::new(),
            created_at: None,
            updated_at: None,
        }
    }

    /// Sets the document fields.
    #[must_use]
    pub fn fields(mut self, fields: impl Into<DocumentFields>) -> Self {
        self.fields = fields.into();

        self
    }

    /// Sets the document's creation timestamp.
    #[must_use]
    pub fn created_at(mut self, ts: Option<impl Into<String>>) -> Self {
        self.created_at = ts.map(std::convert::Into::into);

        self
    }

    /// Sets the document's last update timestamp.
    #[must_use]
    pub fn updated_at(mut self, ts: Option<impl Into<String>>) -> Self {
        self.updated_at = ts.map(std::convert::Into::into);

        self
    }

    /// Builds and returns the `Document`.
    #[must_use]
    pub fn build(self) -> Document {
        Document {
            id: self.id,
            fields: self.fields,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;

    use super::*;

    #[test]
    fn new_creates_empty_document() {
        let doc = Document::new("abc123");
        assert_eq!(doc.id, "abc123");
        assert!(doc.fields.is_empty());
        assert!(doc.created_at.is_none());
        assert!(doc.updated_at.is_none());
    }

    #[test]
    fn get_returns_field_value() {
        let mut doc = Document::new("id1");
        doc.fields.insert("title".to_string(), json!("Hello"));
        assert_eq!(doc.get("title"), Some(&json!("Hello")));
        assert_eq!(doc.get("missing"), None);
    }

    #[test]
    fn get_str_returns_string_value() {
        let mut doc = Document::new("id1");
        doc.fields.insert("title".to_string(), json!("Hello"));
        doc.fields.insert("count".to_string(), json!(42));
        assert_eq!(doc.get_str("title"), Some("Hello"));
        assert_eq!(doc.get_str("count"), None); // not a string
        assert_eq!(doc.get_str("missing"), None);
    }

    #[test]
    fn strip_fields_top_level() {
        let mut doc = Document::new("d1");
        doc.fields.insert("title".into(), json!("Hello"));
        doc.fields.insert("secret".into(), json!("hidden"));

        doc.strip_fields(&["secret".into()]);

        assert!(doc.fields.contains_key("title"));
        assert!(!doc.fields.contains_key("secret"));
    }

    #[test]
    fn strip_fields_flat_group_subfield() {
        let mut doc = Document::new("d1");
        doc.fields.insert("address__city".into(), json!("Berlin"));
        doc.fields.insert("address__zip".into(), json!("10115"));

        doc.strip_fields(&["address__city".into()]);

        assert!(!doc.fields.contains_key("address__city"));
        assert!(doc.fields.contains_key("address__zip"));
    }

    #[test]
    fn strip_fields_nested_group_post_hydration() {
        let mut doc = Document::new("d1");
        doc.fields
            .insert("address".into(), json!({"city": "Berlin", "zip": "10115"}));

        doc.strip_fields(&["address__city".into()]);

        let addr = doc.fields.get("address").unwrap();
        assert!(addr.get("zip").is_some());
        assert!(addr.get("city").is_none());
    }

    #[test]
    fn strip_fields_deeply_nested_group() {
        let mut doc = Document::new("d1");
        doc.fields.insert(
            "meta".into(),
            json!({"address": {"city": "Berlin", "zip": "10115"}}),
        );

        doc.strip_fields(&["meta__address__city".into()]);

        let addr = doc.fields.get("meta").unwrap().get("address").unwrap();
        assert!(addr.get("zip").is_some());
        assert!(addr.get("city").is_none());
    }

    #[test]
    fn strip_fields_nonexistent_is_noop() {
        let mut doc = Document::new("d1");
        doc.fields.insert("title".into(), json!("Hello"));

        doc.strip_fields(&["nonexistent".into(), "group__nonexistent".into()]);

        assert_eq!(doc.fields.len(), 1);
    }

    #[test]
    fn builds_document_with_fields() {
        let mut fields = HashMap::new();
        fields.insert("title".to_string(), json!("Hello"));
        let doc = DocumentBuilder::new("doc-1")
            .fields(fields)
            .created_at(Some("2024-01-01"))
            .updated_at(Some("2024-01-02"))
            .build();
        assert_eq!(doc.id, "doc-1");
        assert_eq!(doc.fields.get("title"), Some(&json!("Hello")));
        assert_eq!(doc.created_at.as_deref(), Some("2024-01-01"));
        assert_eq!(doc.updated_at.as_deref(), Some("2024-01-02"));
    }

    #[test]
    fn builds_document_minimal() {
        let doc = DocumentBuilder::new("minimal").build();
        assert_eq!(doc.id, "minimal");
        assert!(doc.fields.is_empty());
        assert!(doc.created_at.is_none());
        assert!(doc.updated_at.is_none());
    }
}
