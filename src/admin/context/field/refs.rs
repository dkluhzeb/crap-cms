//! Reference field variants: Relationship, Upload, Join.
//!
//! These point to other documents/collections. Relationship and Upload share
//! the `relationship_collection` + `picker` shape; Join exposes its inverse
//! reference.

use serde::Serialize;

use super::BaseFieldData;

// ── Relationship ──────────────────────────────────────────────────

/// Relationship to documents in another collection. The `selected_items`
/// field is `None` after the build phase and `Some` after enrichment.
#[derive(Serialize)]
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

    /// Selected items resolved from the DB during enrichment. For
    /// polymorphic relationships this is a `Vec<SelectedCollectionItem>`
    /// (each item carries its collection); otherwise just `{id, label}`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_items: Option<RelationshipSelected>,
}

/// Either a flat list of `{id, label}` or, for polymorphic relationships,
/// items that also carry their target collection.
#[derive(Serialize)]
#[serde(untagged)]
pub enum RelationshipSelected {
    Flat(Vec<RelationshipSelectedItem>),
    Polymorphic(Vec<SelectedCollectionItem>),
}

/// One row of a non-polymorphic `selected_items` list.
#[derive(Serialize)]
pub struct RelationshipSelectedItem {
    pub id: String,
    pub label: String,
}

/// One row of a polymorphic `selected_items` list — same as
/// [`RelationshipSelectedItem`] but with the target collection attached so
/// templates can render labels like `{collection} / {label}`.
#[derive(Serialize)]
pub struct SelectedCollectionItem {
    pub id: String,
    pub label: String,
    pub collection: String,
}

// ── Upload ────────────────────────────────────────────────────────

/// Upload reference (specialised relationship to a media collection).
#[derive(Serialize)]
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
}

// ── Join ──────────────────────────────────────────────────────────

/// Read-only inverse-reference field. The `readonly` flag on
/// [`BaseFieldData`] is set to `true` for join fields.
#[derive(Serialize)]
pub struct JoinField {
    #[serde(flatten)]
    pub base: BaseFieldData,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub join_collection: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub join_on: Option<String>,
}
