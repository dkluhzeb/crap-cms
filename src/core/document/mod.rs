//! Document types for core CMS content, including the main [`Document`] type
//! and [`VersionSnapshot`] for versioning and draft support.

mod document_builder;
pub mod version_snapshot;
pub mod version_snapshot_builder;

pub use document_builder::DocumentBuilder;
pub use version_snapshot::VersionSnapshot;
pub use version_snapshot_builder::VersionSnapshotBuilder;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A single content document with an ID, user-defined fields, and optional timestamps.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    /// The unique identifier for this document.
    pub id: String,
    /// A map of field names to their JSON-serialized values.
    #[serde(flatten)]
    pub fields: HashMap<String, serde_json::Value>,
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
    pub fn new(id: String) -> Self {
        Self {
            id,
            fields: HashMap::new(),
            created_at: None,
            updated_at: None,
        }
    }

    /// Returns a new `DocumentBuilder` for constructing a document with the given ID.
    pub fn builder(id: impl Into<String>) -> DocumentBuilder {
        DocumentBuilder::new(id)
    }

    /// Get a field value by name.
    pub fn get(&self, key: &str) -> Option<&serde_json::Value> {
        self.fields.get(key)
    }

    /// Get a field value as a string.
    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.fields.get(key).and_then(|v| v.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_creates_empty_document() {
        let doc = Document::new("abc123".to_string());
        assert_eq!(doc.id, "abc123");
        assert!(doc.fields.is_empty());
        assert!(doc.created_at.is_none());
        assert!(doc.updated_at.is_none());
    }

    #[test]
    fn get_returns_field_value() {
        let mut doc = Document::new("id1".to_string());
        doc.fields
            .insert("title".to_string(), serde_json::json!("Hello"));
        assert_eq!(doc.get("title"), Some(&serde_json::json!("Hello")));
        assert_eq!(doc.get("missing"), None);
    }

    #[test]
    fn get_str_returns_string_value() {
        let mut doc = Document::new("id1".to_string());
        doc.fields
            .insert("title".to_string(), serde_json::json!("Hello"));
        doc.fields
            .insert("count".to_string(), serde_json::json!(42));
        assert_eq!(doc.get_str("title"), Some("Hello"));
        assert_eq!(doc.get_str("count"), None); // not a string
        assert_eq!(doc.get_str("missing"), None);
    }
}
