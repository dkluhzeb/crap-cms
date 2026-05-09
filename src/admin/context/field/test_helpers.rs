//! Shared `#[cfg(test)]` fixtures for the `field/*` parity tests.
//!
//! Each per-variant test module (`base`, `scalars`, `refs`, `composites`)
//! constructs a representative instance and asserts the JSON shape its
//! `Serialize` impl produces. They share [`make_base`] for the common
//! [`BaseFieldData`] portion.

use serde_json::{Map, Value};

use super::{BaseFieldData, ConditionData, ValidationAttrs};

/// Build a [`BaseFieldData`] with sensible defaults. The variant
/// discriminator is provided by the [`FieldContext`](super::FieldContext)
/// wrapper at the call site (internally tagged), so this helper only takes
/// the field name.
pub(super) fn make_base(name: &str) -> BaseFieldData {
    BaseFieldData {
        name: name.to_string(),
        field_name: name.to_string(),
        label: name.to_string(),
        required: false,
        value: Value::String(String::new()),
        placeholder: None,
        description: None,
        readonly: false,
        localized: false,
        locale_locked: false,
        position: None,
        template: None,
        extra: Map::new(),
        error: None,
        validation: ValidationAttrs::default(),
        condition: ConditionData::default(),
    }
}
