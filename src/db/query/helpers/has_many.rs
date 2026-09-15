//! Scalar has-many list encoding (write) and parsing (read).

use serde_json::Value;

use crate::{
    core::{FieldType, parse_number},
    db::{DbValue, types::real_to_json_number},
};

/// Canonicalize a **scalar has-many** field's value into the JSON-array TEXT
/// stored in its column (see [`FieldDefinition::is_has_many_scalar`]).
///
/// Accepts the two shapes that reach the write edge — a typed `Value::Array`
/// (gRPC / Lua / MCP) and a JSON-array *string* (admin form, pre-normalized by
/// `transform_select_has_many`) — and maps every element to the field's own type
/// so the stored element types are identical regardless of ingress surface:
/// `Number` → JSON number (whole values as integers, non-numeric elements
/// dropped), every other scalar (`Text` / `Select` / `Radio`) → string. A null
/// value stores SQL `NULL`; any present value stores a JSON array (`[]` when
/// empty). The read path reverses this via [`parse_has_many_scalar`].
///
/// [`FieldDefinition::is_has_many_scalar`]: crate::core::FieldDefinition::is_has_many_scalar
pub(crate) fn coerce_has_many_scalar(field_type: &FieldType, val: &Value) -> DbValue {
    if val.is_null() {
        return DbValue::Null;
    }

    let elements = match val {
        Value::Array(arr) => arr.clone(),
        Value::String(s) => serde_json::from_str::<Vec<Value>>(s).unwrap_or_default(),
        _ => Vec::new(),
    };

    let canonical: Vec<Value> = elements
        .iter()
        .filter_map(|el| canonical_has_many_element(field_type, el))
        .collect();

    DbValue::Text(Value::Array(canonical).to_string())
}

/// The number a Number value holds — a single value or a has-many element: a
/// JSON number, or a number string read by [`parse_number`] (surrounding
/// whitespace ignored). The one reading validation and the write share.
pub(crate) fn number_element(el: &Value) -> Option<f64> {
    match el {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => parse_number(s),
        _ => None,
    }
}

/// Map one has-many element to the field's canonical JSON type, or `None` to
/// drop it (a non-numeric element in a `Number` list, or a null), mirroring the
/// single-value coercion's "invalid ⇒ dropped" rule.
fn canonical_has_many_element(field_type: &FieldType, el: &Value) -> Option<Value> {
    if el.is_null() {
        return None;
    }

    if *field_type == FieldType::Number {
        let n = number_element(el)?;

        return n.is_finite().then(|| real_to_json_number(n));
    }

    // Text / Select / Radio → string form.
    let s = match el {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };

    Some(Value::String(s))
}

/// Parse a **scalar has-many** column's stored TEXT back into a typed JSON array
/// on read — the inverse of [`coerce_has_many_scalar`]. `row_to_document` is
/// type-blind and yields the raw string, so every read surface would otherwise
/// see `"[1,2]"` instead of `[1, 2]`. A SQL `NULL` (absent value) stays `Null`;
/// a malformed value falls back to an empty array so a reader never sees the raw
/// string.
#[must_use]
pub(crate) fn parse_has_many_scalar(field_type: &FieldType, val: &Value) -> Value {
    let raw = match val {
        Value::Null => return Value::Null,
        Value::Array(_) => return val.clone(),
        Value::String(s) => s,
        _ => return Value::Array(Vec::new()),
    };

    let Ok(elements) = serde_json::from_str::<Vec<Value>>(raw) else {
        return Value::Array(Vec::new());
    };

    let canonical = elements
        .iter()
        .filter_map(|el| canonical_has_many_element(field_type, el))
        .collect();

    Value::Array(canonical)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    // ── scalar has-many coercion (write) + parse (read) ─────────────────

    fn stored(field_type: &FieldType, v: &Value) -> String {
        match coerce_has_many_scalar(field_type, v) {
            DbValue::Text(s) => s,
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn has_many_number_typed_array_stays_numeric() {
        assert_eq!(stored(&FieldType::Number, &json!([1, 2, 3])), "[1,2,3]");
    }

    /// The admin form pre-normalizes to a JSON array of *strings*; a Number list
    /// must be re-typed to numbers so it matches the API-written shape.
    #[test]
    fn has_many_number_stringified_elements_become_numbers() {
        assert_eq!(stored(&FieldType::Number, &json!(["1", "2"])), "[1,2]");
    }

    /// Admin sends the whole value as a JSON-array *string*.
    #[test]
    fn has_many_number_json_string_input_is_parsed() {
        assert_eq!(stored(&FieldType::Number, &json!("[1, 2]")), "[1,2]");
    }

    #[test]
    fn has_many_number_whole_floats_serialize_as_integers() {
        assert_eq!(stored(&FieldType::Number, &json!([1.0, 2.5])), "[1,2.5]");
    }

    #[test]
    fn has_many_number_drops_non_numeric_elements() {
        assert_eq!(
            stored(&FieldType::Number, &json!([1, "x", null, 3])),
            "[1,3]"
        );
    }

    #[test]
    fn has_many_text_numbers_become_strings() {
        assert_eq!(stored(&FieldType::Text, &json!([1, 2])), r#"["1","2"]"#);
    }

    #[test]
    fn has_many_text_strings_stay_strings() {
        assert_eq!(
            stored(&FieldType::Select, &json!(["a", "b"])),
            r#"["a","b"]"#
        );
    }

    #[test]
    fn has_many_null_stores_sql_null() {
        assert_eq!(
            coerce_has_many_scalar(&FieldType::Number, &Value::Null),
            DbValue::Null
        );
    }

    #[test]
    fn has_many_empty_array_stores_empty_json() {
        assert_eq!(stored(&FieldType::Number, &json!([])), "[]");
    }

    #[test]
    fn parse_has_many_number_string_to_typed_array() {
        assert_eq!(
            parse_has_many_scalar(&FieldType::Number, &json!("[1,2,3]")),
            json!([1, 2, 3])
        );
    }

    #[test]
    fn parse_has_many_text_string_to_typed_array() {
        assert_eq!(
            parse_has_many_scalar(&FieldType::Text, &json!(r#"["a","b"]"#)),
            json!(["a", "b"])
        );
    }

    #[test]
    fn parse_has_many_null_stays_null() {
        assert_eq!(
            parse_has_many_scalar(&FieldType::Number, &Value::Null),
            Value::Null
        );
    }

    #[test]
    fn parse_has_many_array_passes_through() {
        assert_eq!(
            parse_has_many_scalar(&FieldType::Number, &json!([1, 2])),
            json!([1, 2])
        );
    }

    #[test]
    fn parse_has_many_malformed_falls_back_to_empty_array() {
        assert_eq!(
            parse_has_many_scalar(&FieldType::Number, &json!("not json")),
            json!([])
        );
    }

    /// Write then read round-trips to a typed array, agreeing regardless of the
    /// input shape (typed vs admin-stringified).
    #[test]
    fn has_many_write_read_round_trip_agrees_across_shapes() {
        for input in [json!([1, 2]), json!(["1", "2"]), json!("[1,2]")] {
            let DbValue::Text(stored) = coerce_has_many_scalar(&FieldType::Number, &input) else {
                panic!("expected Text");
            };
            let read = parse_has_many_scalar(&FieldType::Number, &Value::String(stored));
            assert_eq!(read, json!([1, 2]), "input {input:?}");
        }
    }
}
