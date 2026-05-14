//! Document context — the typed shape of `{{document.*}}` for edit/delete pages.

use schemars::JsonSchema;
use serde::Serialize;

use crate::{
    core::{Document, DocumentFields},
    typegen::LuaAnnotation,
};

/// A document reference exposed at `{{document.*}}`. The `data` map carries the
/// document's field values (untyped — typing field values is part of 1.C.2).
#[derive(Serialize, JsonSchema)]
pub struct DocumentRef {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<DocumentFields>,
}

impl LuaAnnotation for DocumentRef {
    fn render_lua_annotation(out: &mut String) {
        out.push_str("---@class crap.template.document\n");
        out.push_str("---@field id string\n");
        out.push_str("---@field created_at? string\n");
        out.push_str("---@field updated_at? string\n");
        out.push_str("---@field status? string\n");
        out.push_str("---@field data? table\n");
        out.push('\n');
    }
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
