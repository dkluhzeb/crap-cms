//! The `FieldContext` enum + `base()` / `base_mut()` accessors.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
#[cfg(test)]
use serde_json::Value;

use super::{
    base::BaseFieldData,
    composites::{ArrayField, BlocksField, GroupField, RowField, TabsField},
    refs::{JoinField, RelationshipField, UploadField},
    scalars::{
        CheckboxField, ChoiceField, CodeField, DateField, NumberField, RichtextField, TextField,
        TextareaField,
    },
};

/// Typed admin form field context — one variant per
/// [`FieldType`](crate::core::FieldType).
///
/// Internally tagged on `field_type` (lowercase variant name) so the
/// serialized JSON has `{"field_type": "text", ...flat fields...}`. This is
/// the single source of truth for the discriminator — [`BaseFieldData`]
/// does NOT carry a `field_type` field.
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(tag = "field_type", rename_all = "lowercase")]
#[schemars(rename_all = "lowercase")]
pub enum FieldContext {
    /// Plain text input (or tag input when `has_many`).
    Text(TextField),
    /// Email address input (validated client-side as `type=email`).
    Email(TextField),
    /// Password input — synthetic, used for auth-collection forms.
    Password(TextField),
    /// Free-form JSON input.
    Json(TextField),
    /// Multi-line textarea.
    Textarea(TextareaField),
    /// Numeric input (or tag input when `has_many`).
    Number(NumberField),
    /// Source-code editor (CodeMirror).
    Code(CodeField),
    /// Rich-text editor (ProseMirror).
    Richtext(RichtextField),
    /// Date / datetime picker.
    Date(DateField),
    /// Single boolean checkbox.
    Checkbox(CheckboxField),
    /// Select dropdown.
    Select(ChoiceField),
    /// Radio button group.
    Radio(ChoiceField),
    /// Reference to another collection's documents.
    Relationship(RelationshipField),
    /// Upload field (specialised relationship to media collection).
    Upload(UploadField),
    /// Read-only join field (computed inverse relationship).
    Join(JoinField),
    /// Inline group of sub-fields (with `__` column-name prefix).
    Group(GroupField),
    /// Layout-only row wrapper (transparent — no name added).
    Row(RowField),
    /// Layout collapsible wrapper (transparent + `collapsed`).
    Collapsible(GroupField),
    /// Layout tabbed wrapper (each tab has its own sub-fields).
    Tabs(TabsField),
    /// Repeating array of homogeneous rows.
    Array(ArrayField),
    /// Repeating array of heterogeneous block-typed rows.
    Blocks(BlocksField),
}

impl FieldContext {
    /// Convert this field context to its JSON representation. Infallible —
    /// admin context structs serialize cleanly. Test-only: production code
    /// serializes via the typed pipeline, but tests use this for assertions
    /// against the wire JSON shape.
    #[cfg(test)]
    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).expect("FieldContext serialization is infallible")
    }

    /// Borrow the shared base data of this field context, regardless of variant.
    pub fn base(&self) -> &BaseFieldData {
        match self {
            FieldContext::Text(f)
            | FieldContext::Email(f)
            | FieldContext::Password(f)
            | FieldContext::Json(f) => &f.base,
            FieldContext::Textarea(f) => &f.base,
            FieldContext::Number(f) => &f.base,
            FieldContext::Code(f) => &f.base,
            FieldContext::Richtext(f) => &f.base,
            FieldContext::Date(f) => &f.base,
            FieldContext::Checkbox(f) => &f.base,
            FieldContext::Select(f) | FieldContext::Radio(f) => &f.base,
            FieldContext::Relationship(f) => &f.base,
            FieldContext::Upload(f) => &f.base,
            FieldContext::Join(f) => &f.base,
            FieldContext::Group(f) | FieldContext::Collapsible(f) => &f.base,
            FieldContext::Row(f) => &f.base,
            FieldContext::Tabs(f) => &f.base,
            FieldContext::Array(f) => &f.base,
            FieldContext::Blocks(f) => &f.base,
        }
    }

    /// Mutably borrow the shared base data. Used by post-build enrichers
    /// (display conditions, error injection) that need to mutate base
    /// fields without caring about the variant.
    pub fn base_mut(&mut self) -> &mut BaseFieldData {
        match self {
            FieldContext::Text(f)
            | FieldContext::Email(f)
            | FieldContext::Password(f)
            | FieldContext::Json(f) => &mut f.base,
            FieldContext::Textarea(f) => &mut f.base,
            FieldContext::Number(f) => &mut f.base,
            FieldContext::Code(f) => &mut f.base,
            FieldContext::Richtext(f) => &mut f.base,
            FieldContext::Date(f) => &mut f.base,
            FieldContext::Checkbox(f) => &mut f.base,
            FieldContext::Select(f) | FieldContext::Radio(f) => &mut f.base,
            FieldContext::Relationship(f) => &mut f.base,
            FieldContext::Upload(f) => &mut f.base,
            FieldContext::Join(f) => &mut f.base,
            FieldContext::Group(f) | FieldContext::Collapsible(f) => &mut f.base,
            FieldContext::Row(f) => &mut f.base,
            FieldContext::Tabs(f) => &mut f.base,
            FieldContext::Array(f) => &mut f.base,
            FieldContext::Blocks(f) => &mut f.base,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_helpers::make_base;
    use super::*;

    /// The internally-tagged enum produces `{"field_type": "...", ...flat
    /// keys...}` with no per-variant wrapper object.
    #[test]
    fn untagged_enum_produces_no_variant_wrapper() {
        let f = TextField {
            base: make_base("title"),
            has_many: None,
            tags: None,
        };
        let v = serde_json::to_value(FieldContext::Text(f)).unwrap();
        // Internally tagged: no `{"Text": {...}}` wrapper — the keys are at root.
        assert!(v.is_object());
        assert!(v.get("Text").is_none());
        assert_eq!(v["name"], "title");
    }
}
