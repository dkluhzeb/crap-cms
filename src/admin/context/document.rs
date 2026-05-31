//! Document context — the typed shape of `{{document.*}}` for edit/delete pages.

use schemars::JsonSchema;
use serde::Serialize;

use crate::{
    core::{Document, DocumentFields},
    typegen::LuaAnnotation,
};

/// A document reference exposed at `{{document.*}}`. The `data` map carries the
/// document's field values (untyped — typing field values is part of 1.C.2).
#[derive(Serialize, JsonSchema, LuaAnnotation)]
#[lua(class = "crap.template.document")]
pub struct DocumentRef {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[lua(ty = "table")]
    pub data: Option<DocumentFields>,
}

impl DocumentRef {
    /// Minimal stub used by error re-renders that only know the id.
    pub fn stub(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            created_at: None,
            updated_at: None,
            status: None,
            data: None,
        }
    }

    /// Build from a fully-loaded document with an explicit status string.
    pub fn with_status(doc: &Document, status: impl Into<String>) -> Self {
        Self {
            id: doc.id.to_string(),
            created_at: doc.created_at.clone(),
            updated_at: doc.updated_at.clone(),
            status: Some(status.into()),
            data: Some(doc.fields.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::{Value, json};

    use crate::core::document::DocumentBuilder;

    use super::*;

    #[test]
    fn stub_carries_only_the_id() {
        let r = DocumentRef::stub("doc-1");
        assert_eq!(r.id, "doc-1");
        assert!(r.created_at.is_none());
        assert!(r.updated_at.is_none());
        assert!(r.status.is_none());
        assert!(r.data.is_none());
    }

    #[test]
    fn with_status_copies_metadata_fields_and_data() {
        let map: HashMap<String, Value> = serde_json::from_value(json!({ "title": "Hi" })).unwrap();
        let doc = DocumentBuilder::new("doc-2")
            .fields(map)
            .created_at(Some("c".to_string()))
            .updated_at(Some("u".to_string()))
            .build();

        let r = DocumentRef::with_status(&doc, "published");
        assert_eq!(r.id, "doc-2");
        assert_eq!(r.created_at.as_deref(), Some("c"));
        assert_eq!(r.updated_at.as_deref(), Some("u"));
        assert_eq!(r.status.as_deref(), Some("published"));
        assert_eq!(r.data.unwrap().get_str("title"), Some("Hi"));
    }
}
