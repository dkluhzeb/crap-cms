//! Conversion helpers: document/field/value conversions between Rust types and protobuf.

use std::collections::{BTreeMap, HashMap};

use serde_json::{Map, Number, Value};

use crate::{
    api::content,
    core::{Document, FieldDefinition, FieldType},
    db::{Filter, FilterClause, FilterOp},
};

/// Convert a core `Document` to a protobuf `Document`, mapping all fields to a prost Struct.
pub(super) fn document_to_proto(doc: &Document, collection: &str) -> content::Document {
    let mut fields = prost_types::Struct {
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

/// Convert a `serde_json::Value` to a `prost_types::Value` for protobuf serialization.
pub(super) fn json_to_prost_value(v: &Value) -> prost_types::Value {
    match v {
        Value::Null => prost_types::Value {
            kind: Some(prost_types::value::Kind::NullValue(0)),
        },
        Value::Bool(b) => prost_types::Value {
            kind: Some(prost_types::value::Kind::BoolValue(*b)),
        },
        Value::Number(n) => prost_types::Value {
            kind: Some(prost_types::value::Kind::NumberValue(
                n.as_f64().unwrap_or_else(|| {
                    tracing::warn!("JSON number overflows f64 in gRPC conversion, defaulting to 0");
                    0.0
                }),
            )),
        },
        Value::String(s) => prost_types::Value {
            kind: Some(prost_types::value::Kind::StringValue(s.clone())),
        },
        Value::Array(arr) => {
            let values: Vec<_> = arr.iter().map(json_to_prost_value).collect();
            prost_types::Value {
                kind: Some(prost_types::value::Kind::ListValue(
                    prost_types::ListValue { values },
                )),
            }
        }
        Value::Object(map) => {
            let mut fields = BTreeMap::new();
            for (k, v) in map {
                fields.insert(k.clone(), json_to_prost_value(v));
            }
            prost_types::Value {
                kind: Some(prost_types::value::Kind::StructValue(prost_types::Struct {
                    fields,
                })),
            }
        }
    }
}

/// Convert a prost Struct to a flat `HashMap<String, String>`, coercing all values to strings.
pub(super) fn prost_struct_to_hashmap(s: &prost_types::Struct) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for (k, v) in &s.fields {
        let value_str = match &v.kind {
            Some(prost_types::value::Kind::StringValue(s)) => s.clone(),
            Some(prost_types::value::Kind::NumberValue(n)) => n.to_string(),
            Some(prost_types::value::Kind::BoolValue(b)) => b.to_string(),
            Some(prost_types::value::Kind::NullValue(_)) => String::new(),
            _ => String::from("null"),
        };
        map.insert(k.clone(), value_str);
    }
    map
}

/// Convert a prost Struct to a JSON Value map, preserving arrays and nested objects.
/// Used for extracting join table data (has-many relationships and arrays).
pub(super) fn prost_struct_to_json_map(s: &prost_types::Struct) -> HashMap<String, Value> {
    let mut map = HashMap::new();
    for (k, v) in &s.fields {
        map.insert(k.clone(), prost_value_to_json(v));
    }
    map
}

/// Convert a `prost_types::Value` back to a `serde_json::Value`.
pub(super) fn prost_value_to_json(v: &prost_types::Value) -> Value {
    match &v.kind {
        Some(prost_types::value::Kind::NullValue(_)) => Value::Null,
        Some(prost_types::value::Kind::BoolValue(b)) => Value::Bool(*b),
        Some(prost_types::value::Kind::NumberValue(n)) => {
            Number::from_f64(*n).map(Value::Number).unwrap_or_else(|| {
                tracing::warn!(
                    "Non-finite float {} in gRPC response, converting to null",
                    n
                );
                Value::Null
            })
        }
        Some(prost_types::value::Kind::StringValue(s)) => Value::String(s.clone()),
        Some(prost_types::value::Kind::ListValue(list)) => {
            Value::Array(list.values.iter().map(prost_value_to_json).collect())
        }
        Some(prost_types::value::Kind::StructValue(s)) => {
            let obj: Map<String, Value> = s
                .fields
                .iter()
                .map(|(k, v)| (k.clone(), prost_value_to_json(v)))
                .collect();
            Value::Object(obj)
        }
        None => Value::Null,
    }
}

/// Convert a `FieldDefinition` to a protobuf `FieldInfo`, including options, blocks, and relationship metadata.
pub(super) fn field_def_to_proto(field: &FieldDefinition) -> content::FieldInfo {
    // Tabs stores sub-fields in field.tabs[*].fields, not field.fields.
    // Flatten all tab sub-fields into the proto `fields` list.
    let sub_fields: Vec<_> = if field.field_type == FieldType::Tabs {
        field
            .tabs
            .iter()
            .flat_map(|tab| tab.fields.iter())
            .map(field_def_to_proto)
            .collect()
    } else {
        field.fields.iter().map(field_def_to_proto).collect()
    };

    content::FieldInfo {
        name: field.name.clone(),
        r#type: field.field_type.as_str().to_string(),
        required: field.required,
        unique: field.unique,
        relationship_collection: field
            .relationship
            .as_ref()
            .map(|r| r.collection.to_string()),
        relationship_has_many: field.relationship.as_ref().map(|r| r.has_many),
        options: field
            .options
            .iter()
            .map(|o| content::SelectOptionInfo {
                label: o.label.resolve_default().to_string(),
                value: o.value.clone(),
            })
            .collect(),
        fields: sub_fields,
        relationship_max_depth: field.relationship.as_ref().and_then(|r| r.max_depth),
        blocks: field
            .blocks
            .iter()
            .map(|bd| content::BlockInfo {
                block_type: bd.block_type.clone(),
                label: bd.label.as_ref().map(|ls| ls.resolve_default().to_string()),
                fields: bd.fields.iter().map(field_def_to_proto).collect(),
                group: bd.group.clone(),
                image_url: bd.image_url.clone(),
            })
            .collect(),
        localized: field.localized,
    }
}

/// Parse a JSON `where` clause into `Vec<FilterClause>`.
/// Format: `{ "field": { "op": "value" }, "field2": "simple_value" }`
pub(super) fn parse_where_json(json_str: &str) -> Result<Vec<FilterClause>, String> {
    let obj: Value =
        serde_json::from_str(json_str).map_err(|e| format!("JSON parse error: {}", e))?;

    let map = obj
        .as_object()
        .ok_or_else(|| "where clause must be a JSON object".to_string())?;

    let mut clauses = Vec::new();
    for (field, value) in map {
        if field == "or" {
            let arr = value
                .as_array()
                .ok_or_else(|| "'or' must be an array".to_string())?;
            let mut groups = Vec::new();
            for element in arr {
                let obj = element
                    .as_object()
                    .ok_or_else(|| "'or' elements must be objects".to_string())?;
                let mut group = Vec::new();
                for (f, v) in obj {
                    match v {
                        Value::String(s) => {
                            group.push(Filter {
                                field: f.clone(),
                                op: FilterOp::Equals(s.clone()),
                            });
                        }
                        Value::Number(_) | Value::Bool(_) => {
                            let s = value_to_string(v)
                                .map_err(|e| format!("or field '{}': {}", f, e))?;
                            group.push(Filter {
                                field: f.clone(),
                                op: FilterOp::Equals(s),
                            });
                        }
                        Value::Object(ops) => {
                            for (op_name, op_value) in ops {
                                let op = parse_filter_op(op_name, op_value)
                                    .map_err(|e| format!("or field '{}': {}", f, e))?;
                                group.push(Filter {
                                    field: f.clone(),
                                    op,
                                });
                            }
                        }
                        _ => {
                            return Err(format!(
                                "or field '{}': value must be string, number, boolean, or operator object",
                                f
                            ));
                        }
                    }
                }
                groups.push(group);
            }
            clauses.push(FilterClause::Or(groups));
            continue;
        }

        match value {
            Value::String(s) => {
                clauses.push(FilterClause::Single(Filter {
                    field: field.clone(),
                    op: FilterOp::Equals(s.clone()),
                }));
            }
            Value::Number(_) | Value::Bool(_) => {
                let s = value_to_string(value).map_err(|e| format!("field '{}': {}", field, e))?;
                clauses.push(FilterClause::Single(Filter {
                    field: field.clone(),
                    op: FilterOp::Equals(s),
                }));
            }
            Value::Object(ops) => {
                for (op_name, op_value) in ops {
                    let op = parse_filter_op(op_name, op_value)
                        .map_err(|e| format!("field '{}': {}", field, e))?;
                    clauses.push(FilterClause::Single(Filter {
                        field: field.clone(),
                        op,
                    }));
                }
            }
            _ => {
                return Err(format!(
                    "field '{}': value must be string, number, boolean, or operator object",
                    field
                ));
            }
        }
    }
    Ok(clauses)
}

/// Parse a filter operator name (e.g. "equals", "greater_than") and its JSON value into a `FilterOp`.
pub(super) fn parse_filter_op(op_name: &str, value: &Value) -> Result<FilterOp, String> {
    match op_name {
        "equals" => Ok(FilterOp::Equals(value_to_string(value)?)),
        "not_equals" => Ok(FilterOp::NotEquals(value_to_string(value)?)),
        "like" => Ok(FilterOp::Like(value_to_string(value)?)),
        "contains" => Ok(FilterOp::Contains(value_to_string(value)?)),
        "greater_than" => Ok(FilterOp::GreaterThan(value_to_string(value)?)),
        "less_than" => Ok(FilterOp::LessThan(value_to_string(value)?)),
        "greater_than_or_equal" => Ok(FilterOp::GreaterThanOrEqual(value_to_string(value)?)),
        "less_than_or_equal" => Ok(FilterOp::LessThanOrEqual(value_to_string(value)?)),
        "in" => {
            let arr = value
                .as_array()
                .ok_or_else(|| "'in' operator requires an array".to_string())?;
            let vals: Result<Vec<String>, String> = arr.iter().map(value_to_string).collect();
            Ok(FilterOp::In(vals?))
        }
        "not_in" => {
            let arr = value
                .as_array()
                .ok_or_else(|| "'not_in' operator requires an array".to_string())?;
            let vals: Result<Vec<String>, String> = arr.iter().map(value_to_string).collect();
            Ok(FilterOp::NotIn(vals?))
        }
        "exists" => Ok(FilterOp::Exists),
        "not_exists" => Ok(FilterOp::NotExists),
        _ => Err(format!("unknown operator '{}'", op_name)),
    }
}

/// Convert a JSON value to its string representation. Only supports string, number, and boolean.
pub(super) fn value_to_string(v: &Value) -> Result<String, String> {
    match v {
        Value::String(s) => Ok(s.clone()),
        Value::Number(n) => Ok(n.to_string()),
        Value::Bool(b) => Ok(b.to_string()),
        _ => Err("value must be string, number, or boolean".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Document;
    use crate::core::field::{
        BlockDefinition, FieldDefinition, FieldType, LocalizedString, RelationshipConfig,
        SelectOption,
    };
    use crate::db::query::{FilterClause, FilterOp};
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
        let prost_val = prost_types::Value { kind: None };
        let back = prost_value_to_json(&prost_val);
        assert_eq!(back, json!(null));
    }

    // ── prost_struct_to_hashmap ────────────────────────────────────────────

    #[test]
    fn prost_struct_to_hashmap_mixed_types() {
        let mut fields = BTreeMap::new();
        fields.insert(
            "name".to_string(),
            prost_types::Value {
                kind: Some(prost_types::value::Kind::StringValue("Alice".to_string())),
            },
        );
        fields.insert(
            "age".to_string(),
            prost_types::Value {
                kind: Some(prost_types::value::Kind::NumberValue(30.0)),
            },
        );
        fields.insert(
            "active".to_string(),
            prost_types::Value {
                kind: Some(prost_types::value::Kind::BoolValue(true)),
            },
        );
        fields.insert(
            "nothing".to_string(),
            prost_types::Value {
                kind: Some(prost_types::value::Kind::NullValue(0)),
            },
        );
        let s = prost_types::Struct { fields };
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
            prost_types::Value {
                kind: Some(prost_types::value::Kind::ListValue(
                    prost_types::ListValue { values: vec![] },
                )),
            },
        );
        let s = prost_types::Struct { fields };
        let map = prost_struct_to_hashmap(&s);
        assert_eq!(map.get("list").unwrap(), "null");
    }

    // ── prost_struct_to_json_map ───────────────────────────────────────────

    #[test]
    fn prost_struct_to_json_map_preserves_nested_structure() {
        let mut inner_fields = BTreeMap::new();
        inner_fields.insert(
            "x".to_string(),
            prost_types::Value {
                kind: Some(prost_types::value::Kind::NumberValue(10.0)),
            },
        );

        let mut fields = BTreeMap::new();
        fields.insert(
            "tags".to_string(),
            prost_types::Value {
                kind: Some(prost_types::value::Kind::ListValue(
                    prost_types::ListValue {
                        values: vec![
                            prost_types::Value {
                                kind: Some(prost_types::value::Kind::StringValue("a".to_string())),
                            },
                            prost_types::Value {
                                kind: Some(prost_types::value::Kind::StringValue("b".to_string())),
                            },
                        ],
                    },
                )),
            },
        );
        fields.insert(
            "nested".to_string(),
            prost_types::Value {
                kind: Some(prost_types::value::Kind::StructValue(prost_types::Struct {
                    fields: inner_fields,
                })),
            },
        );

        let s = prost_types::Struct { fields };
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
        assert!(
            matches!(&title.kind, Some(prost_types::value::Kind::StringValue(s)) if s == "Hello")
        );
        let count = &fields.fields["count"];
        assert!(
            matches!(&count.kind, Some(prost_types::value::Kind::NumberValue(n)) if (*n - 42.0).abs() < f64::EPSILON)
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

    // ── field_def_to_proto ─────────────────────────────────────────────────

    fn make_field(name: &str, field_type: FieldType) -> FieldDefinition {
        FieldDefinition::builder(name, field_type).build()
    }

    #[test]
    fn field_def_to_proto_simple_text() {
        let field = FieldDefinition::builder("title", FieldType::Text)
            .required(true)
            .unique(true)
            .build();

        let proto = field_def_to_proto(&field);
        assert_eq!(proto.name, "title");
        assert_eq!(proto.r#type, "text");
        assert!(proto.required);
        assert!(proto.unique);
        assert!(proto.relationship_collection.is_none());
        assert!(proto.relationship_has_many.is_none());
        assert!(proto.options.is_empty());
        assert!(proto.fields.is_empty());
        assert!(proto.blocks.is_empty());
        assert!(!proto.localized);
    }

    #[test]
    fn field_def_to_proto_with_relationship() {
        let field = FieldDefinition::builder("author", FieldType::Relationship)
            .relationship({
                let mut rc = RelationshipConfig::new("authors", true);
                rc.max_depth = Some(3);
                rc
            })
            .build();

        let proto = field_def_to_proto(&field);
        assert_eq!(proto.r#type, "relationship");
        assert_eq!(proto.relationship_collection.as_deref(), Some("authors"));
        assert_eq!(proto.relationship_has_many, Some(true));
        assert_eq!(proto.relationship_max_depth, Some(3));
    }

    #[test]
    fn field_def_to_proto_with_options() {
        let field = FieldDefinition::builder("status", FieldType::Select)
            .options(vec![
                SelectOption::new(LocalizedString::Plain("Draft".to_string()), "draft"),
                SelectOption::new(LocalizedString::Plain("Published".to_string()), "published"),
            ])
            .build();

        let proto = field_def_to_proto(&field);
        assert_eq!(proto.options.len(), 2);
        assert_eq!(proto.options[0].label, "Draft");
        assert_eq!(proto.options[0].value, "draft");
        assert_eq!(proto.options[1].label, "Published");
        assert_eq!(proto.options[1].value, "published");
    }

    #[test]
    fn field_def_to_proto_with_blocks() {
        let field = FieldDefinition::builder("content", FieldType::Blocks)
            .blocks(vec![{
                let mut bd = BlockDefinition::new(
                    "text_block",
                    vec![make_field("body", FieldType::Textarea)],
                );
                bd.label = Some(LocalizedString::Plain("Text Block".to_string()));
                bd
            }])
            .build();

        let proto = field_def_to_proto(&field);
        assert_eq!(proto.blocks.len(), 1);
        assert_eq!(proto.blocks[0].block_type, "text_block");
        assert_eq!(proto.blocks[0].label.as_deref(), Some("Text Block"));
        assert_eq!(proto.blocks[0].fields.len(), 1);
        assert_eq!(proto.blocks[0].fields[0].name, "body");
        assert_eq!(proto.blocks[0].fields[0].r#type, "textarea");
    }

    #[test]
    fn field_def_to_proto_localized() {
        let field = FieldDefinition::builder("title", FieldType::Text)
            .localized(true)
            .build();

        let proto = field_def_to_proto(&field);
        assert!(proto.localized);
    }

    // ── parse_where_json ───────────────────────────────────────────────────

    #[test]
    fn parse_where_json_simple_equals() {
        let clauses = parse_where_json(r#"{"status": "active"}"#).unwrap();
        assert_eq!(clauses.len(), 1);
        match &clauses[0] {
            FilterClause::Single(f) => {
                assert_eq!(f.field, "status");
                assert!(matches!(&f.op, FilterOp::Equals(v) if v == "active"));
            }
            _ => panic!("expected Single clause"),
        }
    }

    #[test]
    fn parse_where_json_operator_based() {
        let clauses = parse_where_json(r#"{"age": {"greater_than": "18"}}"#).unwrap();
        assert_eq!(clauses.len(), 1);
        match &clauses[0] {
            FilterClause::Single(f) => {
                assert_eq!(f.field, "age");
                assert!(matches!(&f.op, FilterOp::GreaterThan(v) if v == "18"));
            }
            _ => panic!("expected Single clause"),
        }
    }

    #[test]
    fn parse_where_json_or_groups() {
        let input = r#"{
            "or": [
                {"status": "active"},
                {"status": "pending"}
            ]
        }"#;
        let clauses = parse_where_json(input).unwrap();
        assert_eq!(clauses.len(), 1);
        match &clauses[0] {
            FilterClause::Or(groups) => {
                assert_eq!(groups.len(), 2);
                assert_eq!(groups[0].len(), 1);
                assert_eq!(groups[0][0].field, "status");
                assert!(matches!(&groups[0][0].op, FilterOp::Equals(v) if v == "active"));
                assert_eq!(groups[1][0].field, "status");
                assert!(matches!(&groups[1][0].op, FilterOp::Equals(v) if v == "pending"));
            }
            _ => panic!("expected Or clause"),
        }
    }

    #[test]
    fn parse_where_json_or_with_operators() {
        let input = r#"{
            "or": [
                {"age": {"greater_than": "18"}},
                {"role": "admin"}
            ]
        }"#;
        let clauses = parse_where_json(input).unwrap();
        assert_eq!(clauses.len(), 1);
        match &clauses[0] {
            FilterClause::Or(groups) => {
                assert_eq!(groups.len(), 2);
                assert!(matches!(&groups[0][0].op, FilterOp::GreaterThan(v) if v == "18"));
                assert!(matches!(&groups[1][0].op, FilterOp::Equals(v) if v == "admin"));
            }
            _ => panic!("expected Or clause"),
        }
    }

    #[test]
    fn parse_where_json_invalid_json() {
        let result = parse_where_json("not json");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("JSON parse error"));
    }

    #[test]
    fn parse_where_json_non_object() {
        let result = parse_where_json(r#"[1, 2, 3]"#);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("must be a JSON object"));
    }

    #[test]
    fn parse_where_json_invalid_value_type() {
        let result = parse_where_json(r#"{"field": [1, 2]}"#);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .contains("value must be string, number, boolean, or operator object")
        );
    }

    /// Regression: numeric and boolean shorthand values were rejected.
    /// `{"active": true}` and `{"count": 42}` should work as equals filters.
    #[test]
    fn parse_where_json_numeric_and_boolean_shorthand() {
        let clauses = parse_where_json(r#"{"active": true}"#).unwrap();
        assert_eq!(clauses.len(), 1);
        match &clauses[0] {
            FilterClause::Single(f) => {
                assert_eq!(f.field, "active");
                assert!(matches!(&f.op, FilterOp::Equals(v) if v == "true"));
            }
            _ => panic!("Expected single filter"),
        }

        let clauses = parse_where_json(r#"{"count": 42}"#).unwrap();
        assert_eq!(clauses.len(), 1);
        match &clauses[0] {
            FilterClause::Single(f) => {
                assert_eq!(f.field, "count");
                assert!(matches!(&f.op, FilterOp::Equals(v) if v == "42"));
            }
            _ => panic!("Expected single filter"),
        }
    }

    /// Regression: numeric/boolean shorthand values should also work inside `or` groups.
    #[test]
    fn parse_where_json_or_with_numeric_boolean() {
        let input = r#"{"or": [{"active": true}, {"count": 0}]}"#;
        let clauses = parse_where_json(input).unwrap();
        assert_eq!(clauses.len(), 1);
        match &clauses[0] {
            FilterClause::Or(groups) => {
                assert_eq!(groups.len(), 2);
                assert!(matches!(&groups[0][0].op, FilterOp::Equals(v) if v == "true"));
                assert!(matches!(&groups[1][0].op, FilterOp::Equals(v) if v == "0"));
            }
            _ => panic!("Expected Or filter"),
        }
    }

    // ── parse_filter_op ────────────────────────────────────────────────────

    #[test]
    fn parse_filter_op_equals() {
        let op = parse_filter_op("equals", &json!("hello")).unwrap();
        assert!(matches!(op, FilterOp::Equals(v) if v == "hello"));
    }

    #[test]
    fn parse_filter_op_not_equals() {
        let op = parse_filter_op("not_equals", &json!("bye")).unwrap();
        assert!(matches!(op, FilterOp::NotEquals(v) if v == "bye"));
    }

    #[test]
    fn parse_filter_op_like() {
        let op = parse_filter_op("like", &json!("%test%")).unwrap();
        assert!(matches!(op, FilterOp::Like(v) if v == "%test%"));
    }

    #[test]
    fn parse_filter_op_contains() {
        let op = parse_filter_op("contains", &json!("foo")).unwrap();
        assert!(matches!(op, FilterOp::Contains(v) if v == "foo"));
    }

    #[test]
    fn parse_filter_op_comparison_operators() {
        let gt = parse_filter_op("greater_than", &json!("10")).unwrap();
        assert!(matches!(gt, FilterOp::GreaterThan(v) if v == "10"));

        let lt = parse_filter_op("less_than", &json!("5")).unwrap();
        assert!(matches!(lt, FilterOp::LessThan(v) if v == "5"));

        let gte = parse_filter_op("greater_than_or_equal", &json!("10")).unwrap();
        assert!(matches!(gte, FilterOp::GreaterThanOrEqual(v) if v == "10"));

        let lte = parse_filter_op("less_than_or_equal", &json!("5")).unwrap();
        assert!(matches!(lte, FilterOp::LessThanOrEqual(v) if v == "5"));
    }

    #[test]
    fn parse_filter_op_in_with_array() {
        let op = parse_filter_op("in", &json!(["a", "b", "c"])).unwrap();
        match op {
            FilterOp::In(vals) => assert_eq!(vals, vec!["a", "b", "c"]),
            _ => panic!("expected In variant"),
        }
    }

    #[test]
    fn parse_filter_op_not_in_with_array() {
        let op = parse_filter_op("not_in", &json!(["x", "y"])).unwrap();
        match op {
            FilterOp::NotIn(vals) => assert_eq!(vals, vec!["x", "y"]),
            _ => panic!("expected NotIn variant"),
        }
    }

    #[test]
    fn parse_filter_op_in_requires_array() {
        let result = parse_filter_op("in", &json!("not an array"));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("requires an array"));
    }

    #[test]
    fn parse_filter_op_not_in_requires_array() {
        let result = parse_filter_op("not_in", &json!("not an array"));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("requires an array"));
    }

    #[test]
    fn parse_filter_op_exists_and_not_exists() {
        let ex = parse_filter_op("exists", &json!(true)).unwrap();
        assert!(matches!(ex, FilterOp::Exists));

        let nex = parse_filter_op("not_exists", &json!(true)).unwrap();
        assert!(matches!(nex, FilterOp::NotExists));
    }

    #[test]
    fn parse_filter_op_unknown_operator() {
        let result = parse_filter_op("banana", &json!("val"));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown operator 'banana'"));
    }

    // ── value_to_string ────────────────────────────────────────────────────

    #[test]
    fn value_to_string_from_string() {
        assert_eq!(value_to_string(&json!("hello")).unwrap(), "hello");
    }

    #[test]
    fn value_to_string_from_number() {
        assert_eq!(value_to_string(&json!(42)).unwrap(), "42");
        assert_eq!(value_to_string(&json!(3.25)).unwrap(), "3.25");
    }

    #[test]
    fn value_to_string_from_bool() {
        assert_eq!(value_to_string(&json!(true)).unwrap(), "true");
        assert_eq!(value_to_string(&json!(false)).unwrap(), "false");
    }

    #[test]
    fn value_to_string_error_on_array() {
        let result = value_to_string(&json!([1, 2]));
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .contains("must be string, number, or boolean")
        );
    }

    #[test]
    fn value_to_string_error_on_object() {
        let result = value_to_string(&json!({"a": 1}));
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .contains("must be string, number, or boolean")
        );
    }
}
