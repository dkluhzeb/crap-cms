//! Document and value conversion between Rust types and protobuf.

use std::collections::{BTreeMap, HashMap};

use prost_types::{Struct, Value, value::Kind};
use serde_json::{Map, Number, Value as JsonValue};
use tracing::warn;

use crate::{api::content, core::Document};

/// Convert a core `Document` to a protobuf `Document`, mapping all fields to a prost Struct.
pub(in crate::api::handlers) fn document_to_proto(
    doc: &Document,
    collection: &str,
) -> content::Document {
    let mut fields = Struct {
        fields: BTreeMap::new(),
    };

    for (k, v) in &doc.fields {
        fields.fields.insert(k.clone(), json_to_prost_value(v));
    }

    content::Document {
        id: doc.id.to_string(),
        collection: collection.to_string(),
        fields: Some(fields),
        created_at: doc.created_at.clone(),
        updated_at: doc.updated_at.clone(),
    }
}

/// Convert a `serde_json::Value` to a `Value` for protobuf serialization.
pub(in crate::api::handlers) fn json_to_prost_value(v: &JsonValue) -> Value {
    match v {
        JsonValue::Null => Value {
            kind: Some(Kind::NullValue(0)),
        },
        JsonValue::Bool(b) => Value {
            kind: Some(Kind::BoolValue(*b)),
        },
        JsonValue::Number(n) => Value {
            kind: Some(Kind::NumberValue(n.as_f64().unwrap_or_else(|| {
                warn!("JSON number overflows f64 in gRPC conversion, defaulting to 0");
                0.0
            }))),
        },
        JsonValue::String(s) => Value {
            kind: Some(Kind::StringValue(s.clone())),
        },
        JsonValue::Array(arr) => {
            let values: Vec<_> = arr.iter().map(json_to_prost_value).collect();
            Value {
                kind: Some(Kind::ListValue(prost_types::ListValue { values })),
            }
        }
        JsonValue::Object(map) => {
            let mut fields = BTreeMap::new();
            for (k, v) in map {
                fields.insert(k.clone(), json_to_prost_value(v));
            }
            Value {
                kind: Some(Kind::StructValue(Struct { fields })),
            }
        }
    }
}

/// Convert a prost Struct to a flat `HashMap<String, String>`, coercing all values to strings.
pub(in crate::api::handlers) fn prost_struct_to_hashmap(s: &Struct) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for (k, v) in &s.fields {
        let value_str = match &v.kind {
            Some(Kind::StringValue(s)) => s.clone(),
            Some(Kind::NumberValue(n)) => n.to_string(),
            Some(Kind::BoolValue(b)) => b.to_string(),
            Some(Kind::NullValue(_)) => String::new(),
            _ => String::from("null"),
        };
        map.insert(k.clone(), value_str);
    }
    map
}

/// Convert a prost Struct to a JSON Value map, preserving arrays and nested objects.
/// Used for extracting join table data (has-many relationships and arrays).
pub(in crate::api::handlers) fn prost_struct_to_json_map(s: &Struct) -> HashMap<String, JsonValue> {
    let mut map = HashMap::new();

    for (k, v) in &s.fields {
        map.insert(k.clone(), prost_value_to_json(v));
    }

    map
}

/// Convert a `Value` back to a `serde_json::Value`.
pub(in crate::api::handlers) fn prost_value_to_json(v: &Value) -> JsonValue {
    match &v.kind {
        Some(Kind::NullValue(_)) => JsonValue::Null,
        Some(Kind::BoolValue(b)) => JsonValue::Bool(*b),
        Some(Kind::NumberValue(n)) => {
            Number::from_f64(*n)
                .map(JsonValue::Number)
                .unwrap_or_else(|| {
                    warn!(
                        "Non-finite float {} in gRPC response, converting to null",
                        n
                    );
                    JsonValue::Null
                })
        }
        Some(Kind::StringValue(s)) => JsonValue::String(s.clone()),
        Some(Kind::ListValue(list)) => {
            JsonValue::Array(list.values.iter().map(prost_value_to_json).collect())
        }
        Some(Kind::StructValue(s)) => {
            let obj: Map<String, JsonValue> = s
                .fields
                .iter()
                .map(|(k, v)| (k.clone(), prost_value_to_json(v)))
                .collect();
            JsonValue::Object(obj)
        }
        None => JsonValue::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Document;
    use prost_types::{Struct, Value, value::Kind};
    use serde_json::json;
    use std::collections::BTreeMap;

    // ── json_to_prost_value + prost_value_to_json roundtrip ────────────────

    #[test]
    fn roundtrip_null() {
        let json_val = json!(null);
        let prost_val = json_to_prost_value(&json_val);
        let back = prost_value_to_json(&prost_val);
        assert_eq!(back, json_val);
    }

    #[test]
    fn roundtrip_bool() {
        for b in [true, false] {
            let json_val = json!(b);
            let prost_val = json_to_prost_value(&json_val);
            let back = prost_value_to_json(&prost_val);
            assert_eq!(back, json_val);
        }
    }

    #[test]
    fn roundtrip_number() {
        let json_val = json!(42.5);
        let prost_val = json_to_prost_value(&json_val);
        let back = prost_value_to_json(&prost_val);
        assert_eq!(back, json_val);
    }

    #[test]
    fn roundtrip_string() {
        let json_val = json!("hello world");
        let prost_val = json_to_prost_value(&json_val);
        let back = prost_value_to_json(&prost_val);
        assert_eq!(back, json_val);
    }

    #[test]
    fn roundtrip_array() {
        let json_val = json!([1, "two", true, null]);
        let prost_val = json_to_prost_value(&json_val);
        let back = prost_value_to_json(&prost_val);
        // Numbers become f64 in prost, so 1 becomes 1.0
        assert_eq!(back, json!([1.0, "two", true, null]));
    }

    #[test]
    fn roundtrip_object() {
        let json_val = json!({"name": "test", "count": 5, "active": true});
        let prost_val = json_to_prost_value(&json_val);
        let back = prost_value_to_json(&prost_val);
        assert_eq!(back["name"], json!("test"));
        assert_eq!(back["count"], json!(5.0));
        assert_eq!(back["active"], json!(true));
    }

    #[test]
    fn prost_value_to_json_none_kind() {
        let prost_val = Value { kind: None };
        let back = prost_value_to_json(&prost_val);
        assert_eq!(back, json!(null));
    }

    // ── prost_struct_to_hashmap ────────────────────────────────────────────

    #[test]
    fn prost_struct_to_hashmap_mixed_types() {
        let mut fields = BTreeMap::new();
        fields.insert(
            "name".to_string(),
            Value {
                kind: Some(Kind::StringValue("Alice".to_string())),
            },
        );
        fields.insert(
            "age".to_string(),
            Value {
                kind: Some(Kind::NumberValue(30.0)),
            },
        );
        fields.insert(
            "active".to_string(),
            Value {
                kind: Some(Kind::BoolValue(true)),
            },
        );
        fields.insert(
            "nothing".to_string(),
            Value {
                kind: Some(Kind::NullValue(0)),
            },
        );
        let s = Struct { fields };
        let map = prost_struct_to_hashmap(&s);

        assert_eq!(map.get("name").unwrap(), "Alice");
        assert_eq!(map.get("age").unwrap(), "30");
        assert_eq!(map.get("active").unwrap(), "true");
        assert_eq!(map.get("nothing").unwrap(), "");
    }

    #[test]
    fn prost_struct_to_hashmap_unsupported_kind_returns_null() {
        let mut fields = BTreeMap::new();
        // A list value should map to "null" in the hashmap
        fields.insert(
            "list".to_string(),
            Value {
                kind: Some(Kind::ListValue(prost_types::ListValue { values: vec![] })),
            },
        );
        let s = Struct { fields };
        let map = prost_struct_to_hashmap(&s);
        assert_eq!(map.get("list").unwrap(), "null");
    }

    // ── prost_struct_to_json_map ───────────────────────────────────────────

    #[test]
    fn prost_struct_to_json_map_preserves_nested_structure() {
        let mut inner_fields = BTreeMap::new();
        inner_fields.insert(
            "x".to_string(),
            Value {
                kind: Some(Kind::NumberValue(10.0)),
            },
        );

        let mut fields = BTreeMap::new();
        fields.insert(
            "tags".to_string(),
            Value {
                kind: Some(Kind::ListValue(prost_types::ListValue {
                    values: vec![
                        Value {
                            kind: Some(Kind::StringValue("a".to_string())),
                        },
                        Value {
                            kind: Some(Kind::StringValue("b".to_string())),
                        },
                    ],
                })),
            },
        );
        fields.insert(
            "nested".to_string(),
            Value {
                kind: Some(Kind::StructValue(Struct {
                    fields: inner_fields,
                })),
            },
        );

        let s = Struct { fields };
        let map = prost_struct_to_json_map(&s);

        assert_eq!(map.get("tags").unwrap(), &json!(["a", "b"]));
        assert_eq!(map.get("nested").unwrap(), &json!({"x": 10.0}));
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
        assert!(
            matches!(&count.kind, Some(Kind::NumberValue(n)) if (*n - 42.0).abs() < f64::EPSILON)
        );
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
