//! Top-level validation entry point and shared value-shape helpers.

use mlua::Lua;
use serde_json::Value;

use crate::core::{DocumentFields, FieldDefinition, validate::ValidationError};

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
    let mut errors = Vec::new();
    ValidationWalker::new(lua, data, ctx).walk(fields, "", false, &mut errors);

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
