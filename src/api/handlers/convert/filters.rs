//! Filter parsing: JSON `where` clause to `FilterClause` conversion.

use serde_json::Value as JsonValue;

use crate::db::{Filter, FilterClause, FilterOp};

/// Parse a single field's filter value into one or more `Filter` entries.
fn parse_field_filters(field: &str, value: &JsonValue, ctx: &str) -> Result<Vec<Filter>, String> {
    match value {
        JsonValue::String(s) => Ok(vec![Filter {
            field: field.to_string(),
            op: FilterOp::Equals(s.clone()),
        }]),
        JsonValue::Number(_) | JsonValue::Bool(_) => {
            let s = value_to_string(value).map_err(|e| format!("{} '{}': {}", ctx, field, e))?;

            Ok(vec![Filter {
                field: field.to_string(),
                op: FilterOp::Equals(s),
            }])
        }
        JsonValue::Object(ops) => {
            let mut filters = Vec::new();

            for (op_name, op_value) in ops {
                let op = parse_filter_op(op_name, op_value)
                    .map_err(|e| format!("{} '{}': {}", ctx, field, e))?;

                filters.push(Filter {
                    field: field.to_string(),
                    op,
                });
            }

            Ok(filters)
        }
        _ => Err(format!(
            "{} '{}': value must be string, number, boolean, or operator object",
            ctx, field
        )),
    }
}

/// Parse an `or` clause array into grouped filter sets.
fn parse_or_clause(value: &JsonValue) -> Result<FilterClause, String> {
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
            group.extend(parse_field_filters(f, v, "or field")?);
        }

        groups.push(group);
    }

    Ok(FilterClause::Or(groups))
}

/// Parse a JSON `where` clause string into a list of filter clauses.
///
/// Supports simple equality (`{"field": "value"}`), operator objects
/// (`{"field": {"greater_than": 5}}`), and `or` groups.
pub fn parse_where_json(json_str: &str) -> Result<Vec<FilterClause>, String> {
    let obj: JsonValue =
        serde_json::from_str(json_str).map_err(|e| format!("JSON parse error: {}", e))?;

    let map = obj
        .as_object()
        .ok_or_else(|| "where clause must be a JSON object".to_string())?;

    let mut clauses = Vec::new();

    for (field, value) in map {
        if field == "or" {
            clauses.push(parse_or_clause(value)?);
            continue;
        }

        for filter in parse_field_filters(field, value, "field")? {
            clauses.push(FilterClause::Single(filter));
        }
    }

    Ok(clauses)
}

/// Parse a filter operator name (e.g. "equals", "greater_than") and its JSON value into a `FilterOp`.
pub(in crate::api::handlers) fn parse_filter_op(
    op_name: &str,
    value: &JsonValue,
) -> Result<FilterOp, String> {
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
pub(in crate::api::handlers) fn value_to_string(v: &JsonValue) -> Result<String, String> {
    match v {
        JsonValue::String(s) => Ok(s.clone()),
        JsonValue::Number(n) => Ok(n.to_string()),
        JsonValue::Bool(b) => Ok(b.to_string()),
        _ => Err("value must be string, number, or boolean".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::query::{FilterClause, FilterOp};
    use serde_json::json;

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
