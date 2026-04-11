//! Shared helpers for collection CRUD tool implementations.

use std::collections::HashMap;

use serde_json::{Map, Value};
use tracing::warn;

use crate::{core::Document, db::query};

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

/// Parse a scalar filter operator name into a FilterOp.
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

/// Extract flat string data and join data (arrays/objects) from JSON args.
pub(in crate::mcp::tools) fn extract_data_from_args(
    args: &Value,
    skip_keys: &[&str],
) -> (HashMap<String, String>, HashMap<String, Value>) {
    let mut data = HashMap::new();
    let mut join_data = HashMap::new();

    let Some(obj) = args.as_object() else {
        return (data, join_data);
    };

    for (k, v) in obj {
        if skip_keys.contains(&k.as_str()) {
            continue;
        }
        match v {
            Value::String(s) => {
                data.insert(k.clone(), s.clone());
            }
            Value::Number(n) => {
                data.insert(k.clone(), n.to_string());
            }
            Value::Bool(b) => {
                data.insert(k.clone(), bool_to_string(*b));
            }
            Value::Array(_) | Value::Object(_) => {
                join_data.insert(k.clone(), v.clone());
            }
            Value::Null => {}
        }
    }

    (data, join_data)
}
