//! Helpers shared between validation checks.

use serde_json::Value;

/// Decode a `has_many` / join field's element list, accepting both value
/// encodings that reach validation: the typed `Value::Array` shape
/// (Lua/gRPC surfaces) and the JSON-string encoding (admin form ingress).
/// Returns `None` when the value is neither.
pub(crate) fn decode_element_list(value: Option<&Value>) -> Option<Vec<Value>> {
    match value {
        Some(Value::Array(arr)) => Some(arr.clone()),
        Some(Value::String(s)) => serde_json::from_str::<Vec<Value>>(s).ok(),
        _ => None,
    }
}

/// Human-readable form of an element for error messages.
pub(crate) fn element_display(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}
