//! Document and value conversion between Rust types and protobuf.

use std::collections::HashMap;

use serde_json::{Map, Number, Value as JsonValue};
use tracing::warn;

use crate::{
    api::content::{self, DataMap, FieldList, FieldValue, field_value::Kind},
    core::Document,
};

/// Convert a core `Document` to a protobuf `Document`, mapping all fields to a
/// typed `DataMap`.
pub(in crate::api::handlers) fn document_to_proto(
    doc: &Document,
    collection: &str,
) -> content::Document {
    let mut fields = HashMap::new();

    for (k, v) in &doc.fields {
        fields.insert(k.clone(), json_to_field_value(v));
    }

    content::Document {
        id: doc.id.to_string(),
        collection: collection.to_string(),
        fields: Some(DataMap { fields }),
        created_at: doc.created_at.clone(),
        updated_at: doc.updated_at.clone(),
    }
}

/// Map a JSON number to the wire variant that preserves its value: an integer
/// that fits `i64` keeps full precision via `int_value`; a fractional value or
/// an integer outside `i64` range falls back to `double_value` (the former
/// `google.protobuf.Struct` path rounded *every* integer through a double).
fn number_to_kind(n: &Number) -> Kind {
    if let Some(i) = n.as_i64() {
        return Kind::IntValue(i);
    }

    if let Some(f) = n.as_f64() {
        return Kind::DoubleValue(f);
    }

    warn!("JSON number not representable as i64 or f64 in gRPC conversion, defaulting to 0");
    Kind::IntValue(0)
}

/// Convert a `serde_json::Value` to a protobuf `FieldValue`.
pub(in crate::api::handlers) fn json_to_field_value(v: &JsonValue) -> FieldValue {
    let kind = match v {
        JsonValue::Null => Kind::NullValue(0),
        JsonValue::Bool(b) => Kind::BoolValue(*b),
        JsonValue::Number(n) => number_to_kind(n),
        JsonValue::String(s) => Kind::StringValue(s.clone()),
        JsonValue::Array(arr) => Kind::ListValue(FieldList {
            values: arr.iter().map(json_to_field_value).collect(),
        }),
        JsonValue::Object(map) => {
            let mut fields = HashMap::new();
            for (k, v) in map {
                fields.insert(k.clone(), json_to_field_value(v));
            }
            Kind::StructValue(DataMap { fields })
        }
    };

    FieldValue { kind: Some(kind) }
}

/// Convert a protobuf `DataMap` to a JSON `Value` map, preserving arrays and
/// nested objects. Used for extracting join-table data (has-many, arrays).
pub(in crate::api::handlers) fn data_map_to_json_map(dm: &DataMap) -> HashMap<String, JsonValue> {
    dm.fields
        .iter()
        .map(|(k, v)| (k.clone(), field_value_to_json(v)))
        .collect()
}

/// Convert a protobuf `FieldValue` back to a `serde_json::Value`.
pub(in crate::api::handlers) fn field_value_to_json(v: &FieldValue) -> JsonValue {
    match &v.kind {
        Some(Kind::BoolValue(b)) => JsonValue::Bool(*b),
        Some(Kind::IntValue(i)) => JsonValue::Number(Number::from(*i)),
        Some(Kind::DoubleValue(n)) => Number::from_f64(*n).map_or_else(
            || {
                warn!("Non-finite float {n} in gRPC request, converting to null");
                JsonValue::Null
            },
            JsonValue::Number,
        ),
        Some(Kind::StringValue(s)) => JsonValue::String(s.clone()),
        Some(Kind::ListValue(list)) => {
            JsonValue::Array(list.values.iter().map(field_value_to_json).collect())
        }
        Some(Kind::StructValue(dm)) => {
            let obj: Map<String, JsonValue> = dm
                .fields
                .iter()
                .map(|(k, v)| (k.clone(), field_value_to_json(v)))
                .collect();
            JsonValue::Object(obj)
        }
        Some(Kind::NullValue(_)) | None => JsonValue::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Document;
    use serde_json::json;

    // ── json_to_field_value + field_value_to_json roundtrip ────────────────

    fn roundtrip(v: &JsonValue) -> JsonValue {
        field_value_to_json(&json_to_field_value(v))
    }

    #[test]
    fn roundtrip_null() {
        assert_eq!(roundtrip(&json!(null)), json!(null));
    }

    #[test]
    fn roundtrip_bool() {
        for b in [true, false] {
            assert_eq!(roundtrip(&json!(b)), json!(b));
        }
    }

    #[test]
    fn roundtrip_float_stays_double() {
        assert_eq!(roundtrip(&json!(42.5)), json!(42.5));
    }

    /// Integers keep their integer JSON shape (no `.0`), unlike the old
    /// Struct/double path.
    #[test]
    fn roundtrip_integer_stays_integer() {
        assert_eq!(roundtrip(&json!(42)), json!(42));
        assert_eq!(roundtrip(&json!(-7)), json!(-7));
        assert_eq!(roundtrip(&json!(0)), json!(0));
    }

    /// The whole point of the typed value message: an integer above 2^53 that a
    /// double would round now round-trips exactly via `int_value`.
    #[test]
    fn roundtrip_large_integer_keeps_precision() {
        let big = 9_007_199_254_740_993_i64; // 2^53 + 1 — not exactly representable as f64
        let v = json!(big);
        let fv = json_to_field_value(&v);
        assert!(matches!(fv.kind, Some(Kind::IntValue(i)) if i == big));
        assert_eq!(field_value_to_json(&fv), json!(big));
    }

    #[test]
    fn roundtrip_string() {
        assert_eq!(roundtrip(&json!("hello world")), json!("hello world"));
    }

    #[test]
    fn roundtrip_array_preserves_element_types() {
        let v = json!([1, "two", true, null, 3.5]);
        assert_eq!(roundtrip(&v), v);
    }

    #[test]
    fn roundtrip_object_preserves_value_types() {
        let v = json!({"name": "test", "count": 5, "ratio": 0.5, "active": true});
        assert_eq!(roundtrip(&v), v);
    }

    #[test]
    fn field_value_none_kind_is_null() {
        let fv = FieldValue { kind: None };
        assert_eq!(field_value_to_json(&fv), json!(null));
    }

    // ── data_map_to_json_map ───────────────────────────────────────────────

    #[test]
    fn data_map_to_json_map_preserves_nested_structure() {
        let inner = json!({"x": 10});
        let map = json!({"tags": ["a", "b"], "nested": inner});
        let JsonValue::Object(obj) = map else {
            unreachable!()
        };

        let mut fields = HashMap::new();
        for (k, v) in &obj {
            fields.insert(k.clone(), json_to_field_value(v));
        }
        let dm = DataMap { fields };

        let back = data_map_to_json_map(&dm);
        assert_eq!(back.get("tags").unwrap(), &json!(["a", "b"]));
        assert_eq!(back.get("nested").unwrap(), &json!({"x": 10}));
    }

    // ── document_to_proto ──────────────────────────────────────────────────

    #[test]
    fn document_to_proto_with_fields_and_timestamps() {
        let mut doc = Document::new("doc-1".to_string());
        doc.fields.insert("title".to_string(), json!("Hello"));
        doc.fields.insert("count".to_string(), json!(42));
        doc.created_at = Some("2024-01-01T00:00:00Z".to_string());
        doc.updated_at = Some("2024-06-15T12:00:00Z".to_string());

        let proto = document_to_proto(&doc, "posts");

        assert_eq!(proto.id, "doc-1");
        assert_eq!(proto.collection, "posts");
        assert_eq!(proto.created_at.as_deref(), Some("2024-01-01T00:00:00Z"));
        assert_eq!(proto.updated_at.as_deref(), Some("2024-06-15T12:00:00Z"));

        let fields = proto.fields.unwrap();
        let title = &fields.fields["title"];
        assert!(matches!(&title.kind, Some(Kind::StringValue(s)) if s == "Hello"));
        let count = &fields.fields["count"];
        assert!(matches!(&count.kind, Some(Kind::IntValue(42))));
    }

    #[test]
    fn document_to_proto_empty_fields() {
        let doc = Document::new("empty-1".to_string());
        let proto = document_to_proto(&doc, "things");

        assert_eq!(proto.id, "empty-1");
        assert_eq!(proto.collection, "things");
        assert!(proto.created_at.is_none());
        assert!(proto.updated_at.is_none());
        assert!(proto.fields.unwrap().fields.is_empty());
    }
}
