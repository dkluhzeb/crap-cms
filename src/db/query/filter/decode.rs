//! The canonical `where`-clause decoder: one JSON-shaped grammar for every
//! surface.
//!
//! gRPC (`where` JSON string), MCP (`where` tool argument), and Lua CRUD
//! (`where` table, converted to JSON values) all decode through
//! [`decode_where_map`] — previously each surface carried its own copy of
//! this logic and the copies had drifted (MCP rejected `or` groups and
//! silently dropped non-scalar `in` elements; boolean coercion differed).
//!
//! Grammar (per top-level key):
//! - `field = scalar` — string / number / boolean shorthand for `equals`.
//! - `field = { op = value, ... }` — operator object; one filter per entry.
//! - `or = [ { ... }, { ... } ]` — OR across groups; each group is a
//!   field-map `AND`ed internally.
//!
//! Field-name hygiene (system-column rejection, dotted-path normalization)
//! is NOT here — it lives in the shared operation bodies so it runs after
//! decode uniformly.

use serde_json::{Map, Value};

use crate::db::{Filter, FilterClause, FilterOp};

/// Decode a `where` object into filter clauses. Errors are plain strings —
/// each surface wraps them in its own wire error type.
///
/// # Errors
///
/// Returns an error for null/array field values (with a hint toward the
/// operator forms), unknown operators, non-array `in`/`not_in` values, and
/// non-scalar operator values.
pub fn decode_where_map(map: &Map<String, Value>) -> Result<Vec<FilterClause>, String> {
    let mut clauses = Vec::new();

    for (field, value) in map {
        if field == "or" {
            clauses.push(decode_or_clause(value)?);
            continue;
        }

        for filter in decode_field_filters(field, value, "field")? {
            clauses.push(FilterClause::Single(filter));
        }
    }

    Ok(clauses)
}

/// Decode a single field's filter value into one or more `Filter` entries.
fn decode_field_filters(field: &str, value: &Value, ctx: &str) -> Result<Vec<Filter>, String> {
    match value {
        Value::String(s) => Ok(vec![Filter {
            field: field.to_string(),
            op: FilterOp::Equals(s.clone()),
        }]),
        Value::Number(_) | Value::Bool(_) => {
            let s = scalar_to_string(value).map_err(|e| format!("{ctx} '{field}': {e}"))?;

            Ok(vec![Filter {
                field: field.to_string(),
                op: FilterOp::Equals(s),
            }])
        }
        Value::Object(ops) => {
            let mut filters = Vec::new();

            for (op_name, op_value) in ops {
                let op = decode_filter_op(op_name, op_value)
                    .map_err(|e| format!("{ctx} '{field}': {e}"))?;

                filters.push(Filter {
                    field: field.to_string(),
                    op,
                });
            }

            Ok(filters)
        }
        Value::Null => Err(format!(
            "{ctx} '{field}': value must not be null; use the 'exists' / 'not_exists' operator"
        )),
        Value::Array(_) => Err(format!(
            "{ctx} '{field}': value cannot be an array; use an operator \
             (e.g. {{ \"in\": [...] }}) or an 'or' group"
        )),
    }
}

/// Decode an `or` clause array into grouped filter sets.
fn decode_or_clause(value: &Value) -> Result<FilterClause, String> {
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
            group.extend(decode_field_filters(f, v, "or field")?);
        }

        groups.push(group);
    }

    Ok(FilterClause::or_groups(groups))
}

/// Decode an operator name and its JSON value into a `FilterOp`.
fn decode_filter_op(op_name: &str, value: &Value) -> Result<FilterOp, String> {
    // Array / no-value operators — shape-specific value handling. A
    // non-scalar element inside `in`/`not_in` is an ERROR, never silently
    // dropped: a dropped element narrows (or widens, via `not_in`) the match
    // set — dangerous on `delete_many` / `update_many`.
    match op_name {
        "in" => {
            let arr = value
                .as_array()
                .ok_or_else(|| "'in' operator requires an array".to_string())?;
            let vals: Result<Vec<String>, String> = arr.iter().map(scalar_to_string).collect();

            return Ok(FilterOp::In(vals?));
        }
        "not_in" => {
            let arr = value
                .as_array()
                .ok_or_else(|| "'not_in' operator requires an array".to_string())?;
            let vals: Result<Vec<String>, String> = arr.iter().map(scalar_to_string).collect();

            return Ok(FilterOp::NotIn(vals?));
        }
        "exists" => return exists_op(op_name, value, FilterOp::Exists),
        "not_exists" => return exists_op(op_name, value, FilterOp::NotExists),
        _ => {}
    }

    // Scalar operators — the shared canonical grammar.
    FilterOp::scalar_from_name(op_name, scalar_to_string(value)?)
        .ok_or_else(|| format!("unknown operator '{op_name}'"))
}

/// `exists` / `not_exists` take exactly the boolean `true`. `false` (and any
/// non-boolean) is an ERROR: silently ignoring the value would turn
/// `{ exists: false }` into `IS NOT NULL` — the opposite of what the caller
/// meant — and the Lua parser (`hooks::lua_api::crud::filter`) applies the
/// identical rule so every surface agrees.
fn exists_op(op_name: &str, value: &Value, op: FilterOp) -> Result<FilterOp, String> {
    if value == &Value::Bool(true) {
        return Ok(op);
    }

    Err(format!(
        "'{op_name}' operator takes only `true` (got {value}); use 'not_exists' for IS NULL and 'exists' for IS NOT NULL"
    ))
}

/// Convert a scalar JSON value to its canonical filter string. Booleans
/// stringify as `true`/`false`; the SQL edge coerces them per column type
/// (`true` ↔ `1` on Checkbox), so both spellings match stored rows.
fn scalar_to_string(v: &Value) -> Result<String, String> {
    match v {
        Value::String(s) => Ok(s.clone()),
        Value::Number(n) => Ok(n.to_string()),
        Value::Bool(b) => Ok(b.to_string()),
        _ => Err("value must be string, number, or boolean".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn decode(v: &Value) -> Result<Vec<FilterClause>, String> {
        decode_where_map(v.as_object().expect("object"))
    }

    #[test]
    fn scalar_shorthand_becomes_equals() {
        let clauses = decode(&json!({"title": "x", "count": 5, "flag": true})).unwrap();
        assert_eq!(clauses.len(), 3);
        for c in &clauses {
            let FilterClause::Single(f) = c else {
                panic!("expected Single");
            };
            assert!(matches!(&f.op, FilterOp::Equals(_)));
        }
    }

    #[test]
    fn boolean_coerces_to_true_false() {
        let clauses = decode(&json!({"flag": true})).unwrap();
        let FilterClause::Single(f) = &clauses[0] else {
            panic!("expected Single");
        };
        assert!(matches!(&f.op, FilterOp::Equals(v) if v == "true"));
    }

    #[test]
    fn or_groups_decode() {
        let clauses = decode(&json!({
            "or": [
                {"status": "a"},
                {"status": "b", "kind": {"equals": "x"}}
            ]
        }))
        .unwrap();
        assert_eq!(clauses.len(), 1);
        assert!(matches!(&clauses[0], FilterClause::Or(_)));
    }

    #[test]
    fn null_and_array_values_error_with_hints() {
        let err = decode(&json!({"f": null})).unwrap_err();
        assert!(err.contains("exists"), "null hint: {err}");

        let err = decode(&json!({"f": [1, 2]})).unwrap_err();
        assert!(err.contains("in"), "array hint: {err}");
    }

    /// Regression: a non-scalar element inside `in`/`not_in` must ERROR, not
    /// be silently dropped (the old MCP decoder `filter_map`'d them away,
    /// silently changing the match set).
    #[test]
    fn non_scalar_in_element_errors() {
        let err = decode(&json!({"f": {"in": ["a", {"nested": 1}]}})).unwrap_err();
        assert!(err.contains("string, number, or boolean"), "{err}");
    }

    #[test]
    fn unknown_operator_errors() {
        let err = decode(&json!({"f": {"wat": 1}})).unwrap_err();
        assert!(err.contains("unknown operator 'wat'"), "{err}");
    }

    // ── decode_filter_op ────────────────────────────────────────────────────

    #[test]
    fn decode_filter_op_equals() {
        let op = decode_filter_op("equals", &json!("hello")).unwrap();
        assert!(matches!(op, FilterOp::Equals(v) if v == "hello"));
    }

    #[test]
    fn decode_filter_op_not_equals() {
        let op = decode_filter_op("not_equals", &json!("bye")).unwrap();
        assert!(matches!(op, FilterOp::NotEquals(v) if v == "bye"));
    }

    #[test]
    fn decode_filter_op_like() {
        let op = decode_filter_op("like", &json!("%test%")).unwrap();
        assert!(matches!(op, FilterOp::Like(v) if v == "%test%"));
    }

    #[test]
    fn decode_filter_op_contains() {
        let op = decode_filter_op("contains", &json!("foo")).unwrap();
        assert!(matches!(op, FilterOp::Contains(v) if v == "foo"));
    }

    #[test]
    fn decode_filter_op_comparison_operators() {
        let gt = decode_filter_op("greater_than", &json!("10")).unwrap();
        assert!(matches!(gt, FilterOp::GreaterThan(v) if v == "10"));

        let lt = decode_filter_op("less_than", &json!("5")).unwrap();
        assert!(matches!(lt, FilterOp::LessThan(v) if v == "5"));

        let gte = decode_filter_op("greater_than_or_equal", &json!("10")).unwrap();
        assert!(matches!(gte, FilterOp::GreaterThanOrEqual(v) if v == "10"));

        let lte = decode_filter_op("less_than_or_equal", &json!("5")).unwrap();
        assert!(matches!(lte, FilterOp::LessThanOrEqual(v) if v == "5"));
    }

    #[test]
    fn decode_filter_op_in_with_array() {
        let op = decode_filter_op("in", &json!(["a", "b", "c"])).unwrap();
        match op {
            FilterOp::In(vals) => assert_eq!(vals, vec!["a", "b", "c"]),
            _ => panic!("expected In variant"),
        }
    }

    #[test]
    fn decode_filter_op_not_in_with_array() {
        let op = decode_filter_op("not_in", &json!(["x", "y"])).unwrap();
        match op {
            FilterOp::NotIn(vals) => assert_eq!(vals, vec!["x", "y"]),
            _ => panic!("expected NotIn variant"),
        }
    }

    #[test]
    fn decode_filter_op_in_requires_array() {
        let result = decode_filter_op("in", &json!("not an array"));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("requires an array"));
    }

    #[test]
    fn decode_filter_op_not_in_requires_array() {
        let result = decode_filter_op("not_in", &json!("not an array"));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("requires an array"));
    }

    #[test]
    fn decode_filter_op_exists_and_not_exists() {
        let ex = decode_filter_op("exists", &json!(true)).unwrap();
        assert!(matches!(ex, FilterOp::Exists));

        let nex = decode_filter_op("not_exists", &json!(true)).unwrap();
        assert!(matches!(nex, FilterOp::NotExists));
    }

    /// Regression: `exists: false` used to decode as `IS NOT NULL` (value
    /// ignored) — the opposite of the caller's intent. Any value other than
    /// the boolean `true` is now a hard error on both operators.
    #[test]
    fn decode_filter_op_exists_rejects_false_and_non_bool() {
        for op in ["exists", "not_exists"] {
            for bad in [json!(false), json!("true"), json!(1), json!(null)] {
                let err = decode_filter_op(op, &bad).unwrap_err();
                assert!(err.contains("takes only `true`"), "{op} {bad}: {err}");
            }
        }
    }

    #[test]
    fn decode_filter_op_unknown_operator() {
        let result = decode_filter_op("banana", &json!("val"));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown operator 'banana'"));
    }

    // ── scalar_to_string ────────────────────────────────────────────────────

    #[test]
    fn scalar_to_string_from_string() {
        assert_eq!(scalar_to_string(&json!("hello")).unwrap(), "hello");
    }

    #[test]
    fn scalar_to_string_from_number() {
        assert_eq!(scalar_to_string(&json!(42)).unwrap(), "42");
        assert_eq!(scalar_to_string(&json!(3.25)).unwrap(), "3.25");
    }

    #[test]
    fn scalar_to_string_from_bool() {
        assert_eq!(scalar_to_string(&json!(true)).unwrap(), "true");
        assert_eq!(scalar_to_string(&json!(false)).unwrap(), "false");
    }

    #[test]
    fn scalar_to_string_error_on_array() {
        let result = scalar_to_string(&json!([1, 2]));
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .contains("must be string, number, or boolean")
        );
    }

    #[test]
    fn scalar_to_string_error_on_object() {
        let result = scalar_to_string(&json!({"a": 1}));
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .contains("must be string, number, or boolean")
        );
    }
}
