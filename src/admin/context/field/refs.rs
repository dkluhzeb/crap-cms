//! Reference field variants: Relationship, Upload, Join.
//!
//! These point to other documents/collections. Relationship and Upload share
//! the `relationship_collection` + `picker` shape; Join exposes its inverse
//! reference.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::BaseFieldData;

// ── Relationship ──────────────────────────────────────────────────

/// Relationship to documents in another collection. The `selected_items`
/// field is `None` after the build phase and `Some` after enrichment.
#[derive(Serialize, Deserialize, Default, JsonSchema)]
#[serde(default)]
pub struct RelationshipField {
    #[serde(flatten)]
    pub base: BaseFieldData,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub relationship_collection: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_many: Option<bool>,

    /// Set to `Some(true)` for polymorphic relationships (multiple possible
    /// target collections). Templates branch on this to render a collection
    /// picker.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub polymorphic: Option<bool>,

    /// Allowed target collections when `polymorphic` is true.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collections: Option<Vec<String>>,

    /// UI picker style — `"drawer"`, `"inline"`, etc.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub picker: Option<String>,

    /// Selected items resolved from the DB during enrichment.
    /// `collection` is `None` for non-polymorphic relationships and
    /// `Some(target_collection)` for polymorphic ones.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_items: Option<Vec<RelationshipSelectedItem>>,
}

/// One row of a `selected_items` list. For polymorphic relationships the
/// `collection` field is set so templates can render labels like
/// `{collection} / {label}`. Upload `selected_items` reuse this same struct
/// and populate `thumbnail_url`, `is_image`, and `filename`.
#[derive(Serialize, Deserialize, Default, JsonSchema)]
#[serde(default)]
pub struct RelationshipSelectedItem {
    pub id: String,
    pub label: String,

    /// Set only for polymorphic relationships; absent for the common case.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<String>,

    /// Upload-only — preview URL for the upload's thumbnail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail_url: Option<String>,

    /// Upload-only — `Some(true)` when the underlying mime starts with `image/`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_image: Option<bool>,

    /// Upload-only — present when the item came from a has-one upload that
    /// also sets the form's hidden filename input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
}

// ── Upload ────────────────────────────────────────────────────────

/// Upload reference (specialised relationship to a media collection).
#[derive(Serialize, Deserialize, Default, JsonSchema)]
#[serde(default)]
pub struct UploadField {
    #[serde(flatten)]
    pub base: BaseFieldData,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub relationship_collection: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_many: Option<bool>,

    /// UI picker style — defaults to `"drawer"`. Absent when the field
    /// declares `picker = "none"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub picker: Option<String>,

    /// Resolved selected items (after enrichment).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_items: Option<Vec<RelationshipSelectedItem>>,

    /// Has-one only — the resolved filename, populated by enrichment for the
    /// hidden filename input.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_filename: Option<String>,

    /// Has-one only — the resolved thumbnail URL for image previews.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_preview_url: Option<String>,
}

// ── Join ──────────────────────────────────────────────────────────

/// Read-only inverse-reference field. The `readonly` flag on
/// [`BaseFieldData`] is set to `true` for join fields.
#[derive(Serialize, Deserialize, Default, JsonSchema)]
#[serde(default)]
pub struct JoinField {
    #[serde(flatten)]
    pub base: BaseFieldData,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub join_collection: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub join_on: Option<String>,

    /// Reverse-lookup items resolved by enrichment for the join target.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub join_items: Option<Vec<JoinItem>>,

    /// Convenience count of `join_items`. Templates branch on this with
    /// `{{#if join_count}}…{{/if}}`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub join_count: Option<usize>,
}

/// One row of a [`JoinField::join_items`] list — the inverse-reference
/// document's id and display label.
#[derive(Serialize, Deserialize, Default, JsonSchema)]
#[serde(default)]
pub struct JoinItem {
    pub id: String,
    pub label: String,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::admin::context::field::{FieldContext, test_helpers::make_base};

    // ── Relationship ───────────────────────────────────────────────────

    #[test]
    fn relationship_with_polymorphic_emits_collections() {
        let f = RelationshipField {
            base: make_base("author"),
            relationship_collection: Some("users".to_string()),
            has_many: Some(false),
            polymorphic: Some(true),
            collections: Some(vec!["users".to_string(), "guests".to_string()]),
            picker: Some("drawer".to_string()),
            selected_items: None,
        };
        let v = serde_json::to_value(FieldContext::Relationship(f)).unwrap();
        assert_eq!(v["relationship_collection"], "users");
        assert_eq!(v["polymorphic"], true);
        assert_eq!(v["collections"], json!(["users", "guests"]));
    }

    #[test]
    fn relationship_with_selected_items_flat() {
        let f = RelationshipField {
            base: make_base("author"),
            relationship_collection: Some("users".to_string()),
            has_many: Some(true),
            polymorphic: None,
            collections: None,
            picker: None,
            selected_items: Some(vec![RelationshipSelectedItem {
                id: "u1".to_string(),
                label: "Alice".to_string(),
                collection: None,
                thumbnail_url: None,
                is_image: None,
                filename: None,
            }]),
        };
        let v = serde_json::to_value(FieldContext::Relationship(f)).unwrap();
        let items = v["selected_items"].as_array().unwrap();
        assert_eq!(items[0]["id"], "u1");
        assert_eq!(items[0]["label"], "Alice");
        assert!(items[0].get("collection").is_none());
    }

    #[test]
    fn relationship_polymorphic_selected_items_carry_collection() {
        let f = RelationshipField {
            base: make_base("ref"),
            relationship_collection: Some("users".to_string()),
            has_many: Some(false),
            polymorphic: Some(true),
            collections: Some(vec!["users".to_string()]),
            picker: None,
            selected_items: Some(vec![RelationshipSelectedItem {
                id: "u1".to_string(),
                label: "Alice".to_string(),
                collection: Some("users".to_string()),
                thumbnail_url: None,
                is_image: None,
                filename: None,
            }]),
        };
        let v = serde_json::to_value(FieldContext::Relationship(f)).unwrap();
        let items = v["selected_items"].as_array().unwrap();
        assert_eq!(items[0]["collection"], "users");
    }

    // ── Upload ─────────────────────────────────────────────────────────

    #[test]
    fn upload_with_picker_default() {
        let f = UploadField {
            base: make_base("image"),
            relationship_collection: Some("media".to_string()),
            has_many: None,
            picker: Some("drawer".to_string()),
            selected_filename: None,
            selected_preview_url: None,
            selected_items: None,
        };
        let v = serde_json::to_value(FieldContext::Upload(f)).unwrap();
        assert_eq!(v["relationship_collection"], "media");
        assert_eq!(v["picker"], "drawer");
    }

    // ── Join ───────────────────────────────────────────────────────────

    #[test]
    fn join_emits_collection_and_on() {
        let mut b = make_base("posts");
        b.readonly = true;
        let f = JoinField {
            base: b,
            join_collection: Some("posts".to_string()),
            join_on: Some("author_id".to_string()),
            join_items: None,
            join_count: None,
        };
        let v = serde_json::to_value(FieldContext::Join(f)).unwrap();
        assert_eq!(v["readonly"], true);
        assert_eq!(v["join_collection"], "posts");
        assert_eq!(v["join_on"], "author_id");
    }
}
