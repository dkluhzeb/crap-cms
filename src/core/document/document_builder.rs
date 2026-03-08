//! Builders for `crate::core::Document` and `crate::core::document::VersionSnapshot`.

use std::collections::HashMap;

use crate::core::Document;

/// Builder for [`Document`].
pub struct DocumentBuilder {
    id: String,
    fields: HashMap<String, serde_json::Value>,
    created_at: Option<String>,
    updated_at: Option<String>,
}

impl DocumentBuilder {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            fields: HashMap::new(),
            created_at: None,
            updated_at: None,
        }
    }

    pub fn fields(mut self, fields: HashMap<String, serde_json::Value>) -> Self {
        self.fields = fields;
        self
    }

    pub fn created_at(mut self, ts: impl Into<String>) -> Self {
        self.created_at = Some(ts.into());
        self
    }

    pub fn updated_at(mut self, ts: impl Into<String>) -> Self {
        self.updated_at = Some(ts.into());
        self
    }

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
    use super::*;

    #[test]
    fn builds_document_with_fields() {
        let mut fields = HashMap::new();
        fields.insert("title".to_string(), serde_json::json!("Hello"));
        let doc = DocumentBuilder::new("doc-1")
            .fields(fields)
            .created_at("2024-01-01")
            .updated_at("2024-01-02")
            .build();
        assert_eq!(doc.id, "doc-1");
        assert_eq!(doc.fields.get("title"), Some(&serde_json::json!("Hello")));
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
