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
//! - Each [`FieldType`](crate::core::FieldType) variant has a
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

mod base;
mod composites;
mod field_context;
mod refs;
mod scalars;

#[cfg(test)]
mod test_helpers;

pub use base::{BaseFieldData, ConditionData, ValidationAttrs};
pub use composites::{
    ArrayField, ArrayRow, BlockDefinition, BlockRow, BlocksField, GroupField, RowField, TabPanel,
    TabsField,
};
pub use field_context::FieldContext;
pub use refs::{JoinField, JoinItem, RelationshipField, RelationshipSelectedItem, UploadField};
pub use scalars::{
    CheckboxField, ChoiceField, CodeField, DateField, NumberField, RichtextField,
    RichtextNodeAttrCtx, RichtextNodeAttrOption, RichtextNodeDefCtx, SelectOption, TextField,
    TextareaField, TimezoneOption,
};
