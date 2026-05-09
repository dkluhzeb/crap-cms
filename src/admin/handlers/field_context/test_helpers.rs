//! Shared `#[cfg(test)]` fixtures for the `field_context/*` tests.
//!
//! Used by `helpers::tests` and `builder::context::tests`. The
//! [`build_value_contexts`] wrapper is what most legacy tests assert against:
//! the production [`build_field_contexts`] returns typed `FieldContext` values,
//! but tests pin the wire-format JSON shape that templates ultimately consume.

use std::collections::HashMap;

use serde_json::Value;

use crate::{
    admin::{context::field::FieldContext, handlers::field_context::builder::build_field_contexts},
    core::field::{FieldDefinition, FieldType},
};

/// Build a minimal [`FieldDefinition`] with default admin/validation settings.
pub(super) fn make_field(name: &str, ft: FieldType) -> FieldDefinition {
    FieldDefinition::builder(name, ft).build()
}

/// Deserialize JSON fixtures into typed [`FieldContext`] values. Each fixture
/// must include a valid `field_type` tag (e.g. `"text"`, `"group"`, `"tabs"`).
pub(super) fn fields_from_json(values: Vec<Value>) -> Vec<FieldContext> {
    values
        .into_iter()
        .map(|v| {
            serde_json::from_value(v).expect("test fixture must deserialize as a FieldContext")
        })
        .collect()
}

/// Run [`build_field_contexts`] and serialize each typed result to its wire
/// JSON. Tests assert against the wire shape because templates consume JSON,
/// not typed Rust values — this helper preserves that ergonomics.
pub(super) fn build_value_contexts(
    fields: &[FieldDefinition],
    values: &HashMap<String, String>,
    errors: &HashMap<String, String>,
    filter_hidden: bool,
    non_default_locale: bool,
) -> Vec<Value> {
    build_field_contexts(fields, values, errors, filter_hidden, non_default_locale)
        .into_iter()
        .map(|fc| fc.to_value())
        .collect()
}
