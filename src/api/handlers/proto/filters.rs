//! Filter parsing: JSON `where` clause string to `FilterClause` conversion.
//!
//! Thin wire shim: the grammar itself (scalar shorthand, operator objects,
//! `or` groups) lives in the canonical [`decode_where_map`] shared by every
//! surface — this module only owns the JSON-string step.

use crate::db::FilterClause;
use crate::db::query::filter::decode_where_map;

/// Parse a JSON `where` clause string into a list of filter clauses.
///
/// Supports simple equality (`{"field": "value"}`), operator objects
/// (`{"field": {"greater_than": 5}}`), and `or` groups.
///
/// # Errors
///
/// Returns an error when the string is not a JSON object or the object does
/// not decode in the canonical `where` grammar.
pub fn parse_where_json(json_str: &str) -> Result<Vec<FilterClause>, String> {
    let obj: serde_json::Value =
        serde_json::from_str(json_str).map_err(|e| format!("JSON parse error: {e}"))?;

    let map = obj
        .as_object()
        .ok_or_else(|| "where clause must be a JSON object".to_string())?;

    decode_where_map(map)
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
    use proptest::prelude::*;
    use serde_json::Value;

    use super::*;
    use crate::db::{Filter, FilterClause, FilterOp};

    /// Unwrap an OR alternative that is a single filter (the shape `or_groups`
    /// produces for one-field groups, since it collapses them to `Single`).
    fn as_single(clause: &FilterClause) -> &Filter {
        match clause {
            FilterClause::Single(f) => f,
            other => panic!("expected Single alternative, got {other:?}"),
        }
    }

    /// Strategy producing arbitrary JSON values (objects, arrays, scalars,
    /// nested) to exercise the structured parse paths — operator objects,
    /// `or` groups, dotted fields, deep nesting.
    fn arbitrary_json() -> impl Strategy<Value = Value> {
        let leaf = prop_oneof![
            Just(Value::Null),
            any::<bool>().prop_map(Value::Bool),
            any::<i64>().prop_map(Value::from),
            any::<String>().prop_map(Value::String),
        ];
        leaf.prop_recursive(4, 48, 6, |inner| {
            prop_oneof![
                prop::collection::vec(inner.clone(), 0..6).prop_map(Value::Array),
                prop::collection::vec((any::<String>(), inner), 0..6)
                    .prop_map(|kvs| Value::Object(kvs.into_iter().collect())),
            ]
        })
    }

    proptest! {
        /// Property: parsing an arbitrary JSON `where` clause never panics — it
        /// must always return Ok or Err. A panic here would be a DoS via the
        /// untrusted `where` request parameter.
        #[test]
        fn parse_where_json_never_panics_on_arbitrary_json(v in arbitrary_json()) {
            let _ = parse_where_json(&v.to_string());
        }

        /// Property: even raw, possibly-non-JSON input never panics.
        #[test]
        fn parse_where_json_never_panics_on_arbitrary_text(s in any::<String>()) {
            let _ = parse_where_json(&s);
        }
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
            FilterClause::Or(alts) => {
                assert_eq!(alts.len(), 2);
                let (f0, f1) = (as_single(&alts[0]), as_single(&alts[1]));
                assert_eq!(f0.field, "status");
                assert!(matches!(&f0.op, FilterOp::Equals(v) if v == "active"));
                assert_eq!(f1.field, "status");
                assert!(matches!(&f1.op, FilterOp::Equals(v) if v == "pending"));
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
            FilterClause::Or(alts) => {
                assert_eq!(alts.len(), 2);
                assert!(matches!(&as_single(&alts[0]).op, FilterOp::GreaterThan(v) if v == "18"));
                assert!(matches!(&as_single(&alts[1]).op, FilterOp::Equals(v) if v == "admin"));
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
        let result = parse_where_json(r"[1, 2, 3]");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("must be a JSON object"));
    }

    #[test]
    fn parse_where_json_invalid_value_type() {
        let result = parse_where_json(r#"{"field": [1, 2]}"#);
        assert!(result.is_err());
        // Shared-grammar error: bare arrays point toward the operator forms.
        assert!(result.unwrap_err().contains("cannot be an array"));
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
            FilterClause::Or(alts) => {
                assert_eq!(alts.len(), 2);
                assert!(matches!(&as_single(&alts[0]).op, FilterOp::Equals(v) if v == "true"));
                assert!(matches!(&as_single(&alts[1]).op, FilterOp::Equals(v) if v == "0"));
            }
            _ => panic!("Expected Or filter"),
        }
    }
}
