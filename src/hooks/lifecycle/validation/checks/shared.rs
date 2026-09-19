//! Helpers shared between validation checks.

use serde_json::Value;

use crate::core::is_empty_object;

/// Decode a `has_many` / join field's element list, accepting every value
/// encoding that reaches validation: the typed `Value::Array` shape (Lua/gRPC
/// surfaces), the JSON-string encoding (admin form ingress), and the empty
/// object a Lua table with no entries becomes — an empty list, the only list it
/// can mean. Returns `None` when the value is none of these.
pub(crate) fn decode_element_list(value: Option<&Value>) -> Option<Vec<Value>> {
    match value {
        Some(Value::Array(arr)) => Some(arr.clone()),
        Some(Value::String(s)) => serde_json::from_str::<Vec<Value>>(s).ok(),
        Some(value) if is_empty_object(value) => Some(Vec::new()),
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// Regression: an empty has-many list sent from Lua arrives as `{}` — the
    /// shape an empty table takes — and read as "not a list", so the write was
    /// rejected. It is the empty list; an object with entries still is not.
    #[test]
    fn an_empty_object_decodes_as_an_empty_list() {
        assert_eq!(decode_element_list(Some(&json!({}))), Some(Vec::new()));
        assert_eq!(decode_element_list(Some(&json!([]))), Some(Vec::new()));
        assert_eq!(decode_element_list(Some(&json!("[]"))), Some(Vec::new()));
        assert_eq!(decode_element_list(Some(&json!({ "a": 1 }))), None);
        assert_eq!(decode_element_list(Some(&json!(5))), None);
        assert_eq!(decode_element_list(None), None);
    }
}
