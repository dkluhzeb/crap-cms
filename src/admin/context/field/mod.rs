//! Typed `FieldContext` enum modeling the JSON shape produced by
//! [`build_single_field_context`](crate::admin::handlers::field_context::builder).
//!
//! **Status (1.C.2.a):** these types are defined alongside the existing
//! `serde_json::Value`-based builder. The builder still produces `Value` and
//! the enrichment pass still mutates `Value`. This module stages the typed
//! model so 1.C.2.b can migrate the builder to produce `FieldContext`
//! directly.
//!
//! ## Design
//!
//! - Each [`FieldType`](crate::core::field::FieldType) variant has a
//!   corresponding [`FieldContext`] variant.
//! - The enum is `#[serde(untagged)]` — variants serialize to a flat JSON
//!   shape with no `{"type": "Text", "data": {...}}` wrapper. Templates see
//!   the same JSON keys they see today.
//! - A shared [`BaseFieldData`] is `#[serde(flatten)]` into every variant
//!   struct, carrying the common keys (`name`, `field_type`, `label`, etc.).
//! - Type-specific keys live on per-variant structs.
//! - Variants with shape-identical data share a struct (e.g.
//!   `Text`/`Email`/`Password`/`Json` all carry [`TextField`]; `Group` and
//!   `Collapsible` both carry [`GroupField`]).
//!
//! ## Recursive types
//!
//! Composite variants (`Group`, `Row`, `Collapsible`, `Tabs`, `Array`,
//! `Blocks`) hold `Vec<FieldContext>` for their children. The Vec heap
//! indirection makes the enum sized, so no `Box` is needed.

use serde::Serialize;
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
pub use refs::{
    JoinField, RelationshipField, RelationshipSelected, RelationshipSelectedItem,
    SelectedCollectionItem, UploadField,
};
pub use scalars::{
    CheckboxField, ChoiceField, CodeField, DateField, NumberField, RichtextField, SelectOption,
    TextField, TextareaField, TimezoneOption,
};

/// Typed admin form field context — one variant per
/// [`FieldType`](crate::core::field::FieldType).
///
/// Use [`serde_json::to_value`] to get the JSON shape templates consume.
#[derive(Serialize)]
#[serde(untagged)]
pub enum FieldContext {
    /// Plain text input (or tag input when `has_many`).
    Text(TextField),
    /// Email address input (validated client-side as `type=email`).
    Email(TextField),
    /// Password input.
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
}

#[cfg(test)]
mod tests;
