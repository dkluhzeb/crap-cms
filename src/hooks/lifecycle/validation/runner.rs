//! Top-level validation entry point and shared value-shape helpers.

use mlua::Lua;
use serde_json::Value;

use crate::core::{
    DocumentFields, FieldDefinition, flatten_group_fields, validate::ValidationError,
};

use super::ValidationCtx;
use super::recursive::ValidationWalker;

/// Inner implementation of `validate_fields` — operates on a locked `&Lua`.
/// Used by both `HookRunner::validate_fields` and Lua CRUD closures.
pub(crate) fn validate_fields_inner(
    lua: &Lua,
    fields: &[FieldDefinition],
    data: &DocumentFields,
    ctx: &ValidationCtx,
) -> Result<(), ValidationError> {
    // Validation is column-oriented (unique constraints, per-column required,
    // locale columns), so the schema walk runs over a **flat** `group__sub` view
    // — the canonical nested `data` is flattened here (idempotent). The original
    // nested `data` is still threaded through as `document` so user-defined
    // predicates (`required_when`, custom `validate`) see the canonical nested
    // shape, matching field hooks and field access.
    let flat = flatten_group_fields(data, fields);

    let mut errors = Vec::new();
    ValidationWalker::new(lua, &flat, data, ctx).walk(fields, "", false, &mut errors);

    // Document-level: localized required fields must be complete across their
    // `required_locales` (reads the existing row for non-write locales).
    super::check_localized_completeness(lua, fields, &flat, data, ctx, &mut errors);

    if errors.is_empty() {
        Ok(())
    } else {
        Err(ValidationError::new(errors))
    }
}

/// Returns true when `value` represents an absent or blank field — `None`,
/// `Null`, or an empty string. Used identically by every validator that must
/// distinguish empty input from non-empty input (e.g. to skip format checks
/// on empty fields).
pub(in crate::hooks::lifecycle::validation) fn is_empty_value(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => true,
        Some(Value::String(s)) => s.is_empty(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn absent_null_and_empty_string_are_empty() {
        assert!(is_empty_value(None));
        assert!(is_empty_value(Some(&Value::Null)));
        assert!(is_empty_value(Some(&json!(""))));
    }

    #[test]
    fn present_values_are_not_empty() {
        assert!(!is_empty_value(Some(&json!("x"))));
        // Non-string types are never "empty" — including the falsy/zero/blank
        // shapes, so a `0`, `false`, `[]` or `{}` still counts as provided.
        assert!(!is_empty_value(Some(&json!(0))));
        assert!(!is_empty_value(Some(&json!(false))));
        assert!(!is_empty_value(Some(&json!([]))));
        assert!(!is_empty_value(Some(&json!({}))));
        assert!(!is_empty_value(Some(&json!(" "))));
    }
}
