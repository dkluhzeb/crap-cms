//! Typed `FieldContext` enum modeling the JSON shape produced by
//! [`build_single_field_context`](crate::admin::handlers::field_context::builder).
//!
//! ## Status
//!
//! - **Build phase (1.C.2.b)**: produces `FieldContext` end-to-end, then
//!   serializes to `serde_json::Value` at the top-level
//!   [`build_field_contexts`](crate::admin::handlers::field_context::builder::build_field_contexts)
//!   seam.
//! - **Enrichment phase (1.C.2.c, deferred)**: still operates on `&mut Value`.
//!   The typed enum is now bidirectional (Serialize + Deserialize) and ready
//!   for incremental migration as enrichment files are touched.
//!
//! ## Design
//!
//! - Each [`FieldType`](crate::core::field::FieldType) variant has a
//!   corresponding [`FieldContext`] variant.
//! - The enum is `#[serde(tag = "field_type", rename_all = "lowercase")]` —
//!   internally tagged. Serialized JSON has `{"field_type": "text", ...flat
//!   fields...}` with no wrapper.
//! - A shared [`BaseFieldData`] is `#[serde(flatten)]` into every variant
//!   struct, carrying the common keys (`name`, `label`, `value`, …). The
//!   `field_type` discriminator is provided by the enum tag, NOT by base.
//! - Type-specific keys live on per-variant structs.
//! - Variants with shape-identical data share a struct (e.g.
//!   `Text`/`Email`/`Json` all carry [`TextField`]; `Group` and
//!   `Collapsible` both carry [`GroupField`]; `Select`/`Radio` both carry
//!   [`ChoiceField`]).
//! - All field-context types implement both `Serialize` and `Deserialize`
//!   so the enrichment phase can migrate incrementally via Value↔typed
//!   roundtrips when needed.
//!
//! ## Recursive types
//!
//! Composite variants (`Group`, `Row`, `Collapsible`, `Tabs`, `Array`,
//! `Blocks`) hold `Vec<FieldContext>` for their children. The Vec heap
//! indirection makes the enum sized, so no `Box` is needed.

use serde::{Deserialize, Serialize};
use serde_json::Value;

mod base;
mod composites;
mod refs;
mod scalars;

pub use base::{BaseFieldData, ConditionData, ValidationAttrs};
pub use composites::{
    ArrayField, ArrayRow, BlockDefinition, BlockRow, BlocksField, GroupField, RowField, TabPanel,
    TabsField,
};
pub use refs::{JoinField, JoinItem, RelationshipField, RelationshipSelectedItem, UploadField};
pub use scalars::{
    CheckboxField, ChoiceField, CodeField, DateField, NumberField, RichtextField,
    RichtextNodeAttrCtx, RichtextNodeAttrOption, RichtextNodeDefCtx, SelectOption, TextField,
    TextareaField, TimezoneOption,
};

/// Typed admin form field context — one variant per
/// [`FieldType`](crate::core::field::FieldType).
///
/// Internally tagged on `field_type` (lowercase variant name) so the
/// serialized JSON has `{"field_type": "text", ...flat fields...}`. This is
/// the single source of truth for the discriminator — [`BaseFieldData`]
/// does NOT carry a `field_type` field.
#[derive(Serialize, Deserialize)]
#[serde(tag = "field_type", rename_all = "lowercase")]
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
    /// admin context structs serialize cleanly.
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

    /// Returns the canonical lowercase field-type discriminator string for
    /// this variant — same as the `field_type` key in the serialized JSON.
    pub fn field_type_str(&self) -> &'static str {
        match self {
            FieldContext::Text(_) => "text",
            FieldContext::Email(_) => "email",
            FieldContext::Password(_) => "password",
            FieldContext::Json(_) => "json",
            FieldContext::Textarea(_) => "textarea",
            FieldContext::Number(_) => "number",
            FieldContext::Code(_) => "code",
            FieldContext::Richtext(_) => "richtext",
            FieldContext::Date(_) => "date",
            FieldContext::Checkbox(_) => "checkbox",
            FieldContext::Select(_) => "select",
            FieldContext::Radio(_) => "radio",
            FieldContext::Relationship(_) => "relationship",
            FieldContext::Upload(_) => "upload",
            FieldContext::Join(_) => "join",
            FieldContext::Group(_) => "group",
            FieldContext::Row(_) => "row",
            FieldContext::Collapsible(_) => "collapsible",
            FieldContext::Tabs(_) => "tabs",
            FieldContext::Array(_) => "array",
            FieldContext::Blocks(_) => "blocks",
        }
    }
}

#[cfg(test)]
mod tests;
