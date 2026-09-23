//! Decoding the referenced items of a has-many relationship/upload value.

use serde_json::Value;

/// The referenced items in a has-many relationship or upload value.
///
/// One decoder for every encoding a write carries, shared by the validators
/// (required, row bounds, polymorphic allowlist) and the junction writer, so
/// what is judged is what is stored:
///
/// - a typed array (Lua, gRPC, MCP): its elements;
/// - a JSON-array string: its elements;
/// - any other string (the admin form's comma list, or one bare id): its
///   comma-separated, trimmed parts.
///
/// Blank entries (`null`, empty or whitespace-only strings) are dropped.
/// Every other value decodes to no items.
#[must_use]
pub fn reference_items(value: &Value) -> Vec<Value> {
    let items = match value {
        Value::Array(arr) => arr.clone(),
        Value::String(s) => decode_string(s),
        _ => return Vec::new(),
    };

    items.into_iter().filter(|item| !is_blank(item)).collect()
}

/// A string value: a JSON array when it parses as one, otherwise a comma list.
fn decode_string(s: &str) -> Vec<Value> {
    let trimmed = s.trim();

    if trimmed.starts_with('[')
        && let Ok(arr) = serde_json::from_str::<Vec<Value>>(trimmed)
    {
        return arr;
    }

    trimmed
        .split(',')
        .map(|part| Value::String(part.trim().to_string()))
        .collect()
}

fn is_blank(item: &Value) -> bool {
    match item {
        Value::Null => true,
        Value::String(s) => s.trim().is_empty(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn typed_array_yields_its_elements() {
        assert_eq!(
            reference_items(&json!(["a", "b"])),
            vec![json!("a"), json!("b")]
        );
    }

    #[test]
    fn json_array_string_yields_its_elements() {
        assert_eq!(
            reference_items(&json!(r#"["a","b"]"#)),
            vec![json!("a"), json!("b")]
        );
    }

    #[test]
    fn comma_list_yields_trimmed_parts() {
        assert_eq!(
            reference_items(&json!("a, b ,c")),
            vec![json!("a"), json!("b"), json!("c")]
        );
    }

    #[test]
    fn a_bare_id_is_one_item() {
        assert_eq!(reference_items(&json!("a")), vec![json!("a")]);
    }

    #[test]
    fn blank_entries_are_dropped() {
        assert!(reference_items(&json!("")).is_empty());
        assert!(reference_items(&json!(" , ,")).is_empty());
        assert!(reference_items(&json!("[]")).is_empty());
        assert_eq!(reference_items(&json!(["a", null, " "])), vec![json!("a")]);
    }

    #[test]
    fn polymorphic_objects_are_kept() {
        let obj = json!({ "collection": "posts", "id": "p1" });

        assert_eq!(reference_items(&json!([obj.clone()])), vec![obj]);
    }

    #[test]
    fn non_reference_shapes_decode_to_nothing() {
        assert!(reference_items(&Value::Null).is_empty());
        assert!(reference_items(&json!(42)).is_empty());
        assert!(reference_items(&json!({ "a": 1 })).is_empty());
    }
}
