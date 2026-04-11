use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::DocumentId;

use super::DocumentBuilder;

/// A single content document with an ID, user-defined fields, and optional timestamps.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    /// The unique identifier for this document.
    pub id: DocumentId,
    /// A map of field names to their JSON-serialized values.
    #[serde(flatten)]
    pub fields: HashMap<String, Value>,
    /// The timestamp when this document was originally created.
    #[serde(default)]
    pub created_at: Option<String>,
    /// The timestamp when this document was last updated.
    #[serde(default)]
    pub updated_at: Option<String>,
}

#[allow(dead_code)]
impl Document {
    /// Create an empty document with the given ID and no fields or timestamps.
    pub fn new(id: impl Into<DocumentId>) -> Self {
        Self {
            id: id.into(),
            fields: HashMap::new(),
            created_at: None,
            updated_at: None,
        }
    }

    /// Returns a new `DocumentBuilder` for constructing a document with the given ID.
    pub fn builder(id: impl Into<DocumentId>) -> DocumentBuilder {
        DocumentBuilder::new(id)
    }

    /// Get a field value by name.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.fields.get(key)
    }

    /// Get a field value as a string.
    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.fields.get(key).and_then(|v| v.as_str())
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
fn strip_nested(fields: &mut HashMap<String, Value>, segments: &[&str]) {
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

/// Recurse into a serde_json::Map to remove a deeply nested field.
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

#[cfg(test)]
mod tests {
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
}
