//! Shared helpers for collection CRUD tool implementations.

use serde_json::{Map, Value};
use tracing::warn;

use crate::{
    core::{Document, DocumentFields},
    db::query,
};

/// Parse JSON `where` object into filter clauses.
/// Supports `{ field: "value" }` (equals) and `{ field: { op: value } }` (operator-based).
pub(in crate::mcp::tools) fn parse_where_filters(args: &Value) -> Vec<query::FilterClause> {
    let Some(where_obj) = args.get("where").and_then(|v| v.as_object()) else {
        return Vec::new();
    };

    let mut clauses = Vec::new();

    for (field, value) in where_obj {
        match value {
            Value::String(s) => {
                clauses.push(make_equals_clause(field, s.clone()));
            }
            Value::Number(n) => {
                clauses.push(make_equals_clause(field, n.to_string()));
            }
            Value::Bool(b) => {
                clauses.push(make_equals_clause(field, bool_to_string(*b)));
            }
            Value::Object(ops) => {
                parse_operator_filters(field, ops, &mut clauses);
            }
            _ => {}
        }
    }

    clauses
}

/// Create an Equals filter clause for a field.
fn make_equals_clause(field: &str, value: String) -> query::FilterClause {
    query::FilterClause::Single(query::Filter {
        field: field.to_string(),
        op: query::FilterOp::Equals(value),
    })
}

/// Parse operator-based filters: `{ "greater_than": "50", "less_than": "100" }`.
fn parse_operator_filters(
    field: &str,
    ops: &Map<String, Value>,
    clauses: &mut Vec<query::FilterClause>,
) {
    for (op_name, op_value) in ops {
        match op_name.as_str() {
            "in" | "not_in" => {
                let Some(arr) = op_value.as_array() else {
                    continue;
                };
                let vals: Vec<String> = arr
                    .iter()
                    .filter_map(|v| match v {
                        Value::String(s) => Some(s.clone()),
                        Value::Number(n) => Some(n.to_string()),
                        _ => None,
                    })
                    .collect();
                let op = if op_name == "in" {
                    query::FilterOp::In(vals)
                } else {
                    query::FilterOp::NotIn(vals)
                };
                clauses.push(query::FilterClause::Single(query::Filter {
                    field: field.to_string(),
                    op,
                }));
            }
            "exists" => {
                clauses.push(query::FilterClause::Single(query::Filter {
                    field: field.to_string(),
                    op: query::FilterOp::Exists,
                }));
            }
            "not_exists" => {
                clauses.push(query::FilterClause::Single(query::Filter {
                    field: field.to_string(),
                    op: query::FilterOp::NotExists,
                }));
            }
            _ => {
                let val_str = match op_value {
                    Value::String(s) => s.clone(),
                    Value::Number(n) => n.to_string(),
                    Value::Bool(b) => bool_to_string(*b),
                    _ => continue,
                };
                let Some(op) = parse_scalar_op(op_name, val_str) else {
                    continue;
                };
                clauses.push(query::FilterClause::Single(query::Filter {
                    field: field.to_string(),
                    op,
                }));
            }
        }
    }
}

/// Parse a scalar filter operator name into a `FilterOp`.
fn parse_scalar_op(op_name: &str, val: String) -> Option<query::FilterOp> {
    match op_name {
        "equals" => Some(query::FilterOp::Equals(val)),
        "not_equals" => Some(query::FilterOp::NotEquals(val)),
        "contains" => Some(query::FilterOp::Contains(val)),
        "greater_than" => Some(query::FilterOp::GreaterThan(val)),
        "greater_than_equal" | "greater_than_or_equal" => {
            Some(query::FilterOp::GreaterThanOrEqual(val))
        }
        "less_than" => Some(query::FilterOp::LessThan(val)),
        "less_than_equal" | "less_than_or_equal" => Some(query::FilterOp::LessThanOrEqual(val)),
        "like" => Some(query::FilterOp::Like(val)),
        unknown => {
            warn!("Unknown MCP filter operator '{}', skipping", unknown);

            None
        }
    }
}

/// Convert a bool to a SQLite-compatible `"1"` or `"0"` string.
fn bool_to_string(b: bool) -> String {
    if b { "1" } else { "0" }.to_string()
}

/// Convert a Document to a JSON Value.
pub(in crate::mcp::tools) fn doc_to_json(doc: &Document) -> Value {
    let mut obj = Map::new();
    obj.insert("id".to_string(), Value::String(doc.id.to_string()));
    for (k, v) in &doc.fields {
        obj.insert(k.clone(), v.clone());
    }
    if let Some(ref ca) = doc.created_at {
        obj.insert("created_at".to_string(), Value::String(ca.clone()));
    }
    if let Some(ref ua) = doc.updated_at {
        obj.insert("updated_at".to_string(), Value::String(ua.clone()));
    }
    Value::Object(obj)
}

/// Extract typed field data from JSON args, dropping `skip_keys` and `null` values.
/// Scalars and structured values both flow through as `Value` — the typed write
/// pipeline routes them to columns or join tables based on each field's type.
pub(in crate::mcp::tools) fn extract_data_from_args(
    args: &Value,
    skip_keys: &[&str],
) -> DocumentFields {
    let Some(obj) = args.as_object() else {
        return DocumentFields::new();
    };

    obj.iter()
        .filter(|(k, v)| !skip_keys.contains(&k.as_str()) && !v.is_null())
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

#[cfg(test)]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::case_sensitive_file_extension_comparisons,
    clippy::items_after_statements,
    clippy::match_wildcard_for_single_variants,
    clippy::missing_panics_doc,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::unreadable_literal,
    clippy::used_underscore_binding
)]
mod tests {
    use std::collections::HashMap;

    use serde_json::{Map, Value, json};

    use super::*;
    use crate::{
        core::{DocumentFields, DocumentId, document::Document},
        db::query,
    };

    // ── parse_where_filters: array operators ──────────────────────────────

    #[test]
    fn parse_where_in_operator() {
        let args = json!({
            "where": {
                "status": { "in": ["draft", "review"] }
            }
        });
        let clauses = parse_where_filters(&args);
        assert_eq!(clauses.len(), 1);
        match &clauses[0] {
            query::FilterClause::Single(f) => {
                assert_eq!(f.field, "status");
                match &f.op {
                    query::FilterOp::In(vals) => assert_eq!(vals, &["draft", "review"]),
                    other => panic!("Expected In, got {other:?}"),
                }
            }
            other => panic!("Expected Single, got {other:?}"),
        }
    }

    #[test]
    fn parse_where_not_in_operator() {
        let args = json!({
            "where": {
                "role": { "not_in": ["banned", "suspended"] }
            }
        });
        let clauses = parse_where_filters(&args);
        assert_eq!(clauses.len(), 1);
        match &clauses[0] {
            query::FilterClause::Single(f) => {
                assert_eq!(f.field, "role");
                assert!(matches!(&f.op, query::FilterOp::NotIn(_)));
            }
            other => panic!("Expected Single, got {other:?}"),
        }
    }

    #[test]
    fn parse_where_exists_operator() {
        let args = json!({
            "where": {
                "avatar": { "exists": true }
            }
        });
        let clauses = parse_where_filters(&args);
        assert_eq!(clauses.len(), 1);
        match &clauses[0] {
            query::FilterClause::Single(f) => {
                assert_eq!(f.field, "avatar");
                assert!(matches!(&f.op, query::FilterOp::Exists));
            }
            other => panic!("Expected Single, got {other:?}"),
        }
    }

    #[test]
    fn parse_where_not_exists_operator() {
        let args = json!({
            "where": {
                "deleted_at": { "not_exists": true }
            }
        });
        let clauses = parse_where_filters(&args);
        assert_eq!(clauses.len(), 1);
        match &clauses[0] {
            query::FilterClause::Single(f) => {
                assert!(matches!(&f.op, query::FilterOp::NotExists));
            }
            other => panic!("Expected Single, got {other:?}"),
        }
    }

    // ── parse_where_filters: scalar field values ───────────────────────────

    #[test]
    fn parse_where_string_shorthand() {
        // { "field": "value" } → Equals
        let args = json!({ "where": { "title": "hello" } });
        let clauses = parse_where_filters(&args);
        assert_eq!(clauses.len(), 1);
        match &clauses[0] {
            query::FilterClause::Single(f) => {
                assert_eq!(f.field, "title");
                assert!(matches!(&f.op, query::FilterOp::Equals(v) if v == "hello"));
            }
            other => panic!("Expected Single, got {other:?}"),
        }
    }

    #[test]
    fn parse_where_number_shorthand() {
        let args = json!({ "where": { "count": 5 } });
        let clauses = parse_where_filters(&args);
        assert_eq!(clauses.len(), 1);
        match &clauses[0] {
            query::FilterClause::Single(f) => {
                assert_eq!(f.field, "count");
                assert!(matches!(&f.op, query::FilterOp::Equals(v) if v == "5"));
            }
            other => panic!("Expected Single, got {other:?}"),
        }
    }

    #[test]
    fn parse_where_bool_shorthand_true() {
        let args = json!({ "where": { "active": true } });
        let clauses = parse_where_filters(&args);
        assert_eq!(clauses.len(), 1);
        match &clauses[0] {
            query::FilterClause::Single(f) => {
                assert!(matches!(&f.op, query::FilterOp::Equals(v) if v == "1"));
            }
            other => panic!("Expected Single, got {other:?}"),
        }
    }

    #[test]
    fn parse_where_bool_shorthand_false() {
        let args = json!({ "where": { "active": false } });
        let clauses = parse_where_filters(&args);
        assert_eq!(clauses.len(), 1);
        match &clauses[0] {
            query::FilterClause::Single(f) => {
                assert!(matches!(&f.op, query::FilterOp::Equals(v) if v == "0"));
            }
            other => panic!("Expected Single, got {other:?}"),
        }
    }

    #[test]
    fn parse_where_scalar_operators() {
        for (op_name, expected_variant) in &[
            ("not_equals", "not_equals"),
            ("contains", "contains"),
            ("greater_than", "greater_than"),
            ("greater_than_equal", "greater_than_equal"),
            ("less_than", "less_than"),
            ("less_than_equal", "less_than_equal"),
            ("like", "like"),
        ] {
            let args = {
                let mut where_field = Map::new();
                where_field.insert(op_name.to_string(), json!("val"));
                let mut where_obj = Map::new();
                where_obj.insert("field".to_string(), Value::Object(where_field));
                let mut root = Map::new();
                root.insert("where".to_string(), Value::Object(where_obj));
                Value::Object(root)
            };
            let clauses = parse_where_filters(&args);
            assert_eq!(
                clauses.len(),
                1,
                "operator {op_name} produced wrong clause count"
            );
            match &clauses[0] {
                query::FilterClause::Single(f) => {
                    let matched = matches!(
                        (&f.op, *expected_variant),
                        (query::FilterOp::NotEquals(_), "not_equals")
                            | (query::FilterOp::Contains(_), "contains")
                            | (query::FilterOp::GreaterThan(_), "greater_than")
                            | (query::FilterOp::GreaterThanOrEqual(_), "greater_than_equal")
                            | (query::FilterOp::LessThan(_), "less_than")
                            | (query::FilterOp::LessThanOrEqual(_), "less_than_equal")
                            | (query::FilterOp::Like(_), "like")
                    );
                    assert!(
                        matched,
                        "Wrong op variant for operator {}: got {:?}",
                        op_name, f.op
                    );
                }
                other => panic!("Expected Single for {op_name}, got {other:?}"),
            }
        }
    }

    #[test]
    fn parse_where_scalar_op_with_number() {
        let args = json!({ "where": { "age": { "greater_than": 18 } } });
        let clauses = parse_where_filters(&args);
        assert_eq!(clauses.len(), 1);
        match &clauses[0] {
            query::FilterClause::Single(f) => {
                assert!(matches!(&f.op, query::FilterOp::GreaterThan(v) if v == "18"));
            }
            other => panic!("Expected Single, got {other:?}"),
        }
    }

    #[test]
    fn parse_where_scalar_op_with_bool() {
        let args = json!({ "where": { "active": { "equals": true } } });
        let clauses = parse_where_filters(&args);
        assert_eq!(clauses.len(), 1);
        match &clauses[0] {
            query::FilterClause::Single(f) => {
                assert!(matches!(&f.op, query::FilterOp::Equals(v) if v == "1"));
            }
            other => panic!("Expected Single, got {other:?}"),
        }
    }

    #[test]
    fn parse_where_unknown_op_skipped() {
        // Unknown operator name → clause is skipped (no panic)
        let args = json!({ "where": { "field": { "unknown_op": "val" } } });
        let clauses = parse_where_filters(&args);
        assert_eq!(clauses.len(), 0);
    }

    #[test]
    fn parse_where_null_value_skipped() {
        // Null field value → skipped
        let args = json!({ "where": { "field": null } });
        let clauses = parse_where_filters(&args);
        assert_eq!(clauses.len(), 0);
    }

    #[test]
    fn parse_where_null_op_value_skipped() {
        // Op value is null → skipped (neither scalar nor array)
        let args = json!({ "where": { "field": { "equals": null } } });
        let clauses = parse_where_filters(&args);
        assert_eq!(clauses.len(), 0);
    }

    #[test]
    fn parse_where_no_where_key() {
        let args = json!({ "limit": 10 });
        let clauses = parse_where_filters(&args);
        assert!(clauses.is_empty());
    }

    #[test]
    fn parse_where_non_object_where() {
        let args = json!({ "where": "not-an-object" });
        let clauses = parse_where_filters(&args);
        assert!(clauses.is_empty());
    }

    // ── doc_to_json ────────────────────────────────────────────────────────

    #[test]
    fn doc_to_json_includes_all_fields() {
        let mut fields = HashMap::new();
        fields.insert("title".to_string(), json!("Hello"));
        fields.insert("count".to_string(), json!(42));
        let doc = Document {
            id: DocumentId::new("abc123"),
            fields: fields.into(),
            created_at: Some("2024-01-01T00:00:00Z".to_string()),
            updated_at: Some("2024-06-01T00:00:00Z".to_string()),
        };
        let val = doc_to_json(&doc);
        assert_eq!(val["id"], "abc123");
        assert_eq!(val["title"], "Hello");
        assert_eq!(val["count"], 42);
        assert_eq!(val["created_at"], "2024-01-01T00:00:00Z");
        assert_eq!(val["updated_at"], "2024-06-01T00:00:00Z");
    }

    #[test]
    fn doc_to_json_without_timestamps() {
        let doc = Document {
            id: DocumentId::new("xyz"),
            fields: DocumentFields::new(),
            created_at: None,
            updated_at: None,
        };
        let val = doc_to_json(&doc);
        assert_eq!(val["id"], "xyz");
        assert!(val.get("created_at").is_none() || val["created_at"].is_null());
        assert!(val.get("updated_at").is_none() || val["updated_at"].is_null());
    }
}
