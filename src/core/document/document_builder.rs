//! Builders for `crate::core::Document` and `crate::core::document::VersionSnapshot`.

use std::collections::HashMap;

use serde_json::Value;

use crate::core::Document;

/// Builder for [`Document`].
pub struct DocumentBuilder {
    id: String,
    fields: HashMap<String, Value>,
    created_at: Option<String>,
    updated_at: Option<String>,
}

impl DocumentBuilder {
    /// Creates a new `DocumentBuilder` with the specified ID.
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            fields: HashMap::new(),
            created_at: None,
            updated_at: None,
        }
    }

    /// Sets the document fields.
    pub fn fields(mut self, fields: HashMap<String, Value>) -> Self {
        self.fields = fields;
        self
    }

    /// Sets the document's creation timestamp.
    pub fn created_at(mut self, ts: Option<impl Into<String>>) -> Self {
        self.created_at = ts.map(|t| t.into());
        self
    }

    /// Sets the document's last update timestamp.
    pub fn updated_at(mut self, ts: Option<impl Into<String>>) -> Self {
        self.updated_at = ts.map(|t| t.into());
        self
    }

    /// Builds and returns the `Document`.
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
    use serde_json::json;

    use super::*;

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
