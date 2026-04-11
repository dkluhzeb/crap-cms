//! In-memory filter evaluation for `FilterClause` against document data.
//!
//! Used by event stream consumers (SSE, gRPC Subscribe) to enforce row-level
//! access constraints without DB queries. Evaluates the same `FilterClause`
//! types that `Find` uses as SQL WHERE clauses.

use std::collections::HashMap;

use serde_json::Value;

use crate::db::{Filter, FilterClause, FilterOp};

/// Evaluate filter clauses against in-memory document data.
///
/// Returns `true` if all clauses match (AND semantics, same as SQL WHERE).
/// Returns `false` (fail-closed) if a referenced field is missing from data.
/// Returns `true` for empty constraints (no filters = no restrictions).
pub fn matches_constraints(data: &HashMap<String, Value>, constraints: &[FilterClause]) -> bool {
    if constraints.is_empty() {
        return true;
    }

    constraints.iter().all(|clause| match clause {
        FilterClause::Single(filter) => matches_filter(data, filter),
        FilterClause::Or(groups) => groups
            .iter()
            .any(|group| group.iter().all(|filter| matches_filter(data, filter))),
    })
}

/// Evaluate a single filter against document data.
fn matches_filter(data: &HashMap<String, Value>, filter: &Filter) -> bool {
    let value = match data.get(&filter.field) {
        Some(v) => v,
        None => return matches_missing_field(&filter.op),
    };

    let value_str = value_to_string(value);

    match &filter.op {
        FilterOp::Equals(expected) => value_str == *expected,
        FilterOp::NotEquals(expected) => value_str != *expected,
        FilterOp::Contains(needle) => value_str.contains(needle.as_str()),
        FilterOp::Like(pattern) => matches_like(&value_str, pattern),
        FilterOp::GreaterThan(expected) => {
            compare_values(&value_str, expected) == Some(std::cmp::Ordering::Greater)
        }
        FilterOp::LessThan(expected) => {
            compare_values(&value_str, expected) == Some(std::cmp::Ordering::Less)
        }
        FilterOp::GreaterThanOrEqual(expected) => {
            matches!(
                compare_values(&value_str, expected),
                Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal)
            )
        }
        FilterOp::LessThanOrEqual(expected) => {
            matches!(
                compare_values(&value_str, expected),
                Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal)
            )
        }
        FilterOp::In(values) => values.contains(&value_str),
        FilterOp::NotIn(values) => !values.contains(&value_str),
        FilterOp::Exists => true,     // field exists (checked above)
        FilterOp::NotExists => false, // field exists but op says it shouldn't
    }
}

/// Handle filters when the field is missing from data.
/// Fail-closed: missing field means the filter doesn't match,
/// except for `NotExists` which expects the field to be absent.
fn matches_missing_field(op: &FilterOp) -> bool {
    matches!(op, FilterOp::NotExists)
}

/// Convert a JSON value to its string representation for comparison.
fn value_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => if *b { "1" } else { "0" }.to_string(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Compare two string values, trying numeric comparison first.
fn compare_values(a: &str, b: &str) -> Option<std::cmp::Ordering> {
    if let (Ok(na), Ok(nb)) = (a.parse::<f64>(), b.parse::<f64>()) {
        na.partial_cmp(&nb)
    } else {
        Some(a.cmp(b))
    }
}

/// Simple LIKE pattern matching (SQL-style: % = any chars, _ = single char).
fn matches_like(value: &str, pattern: &str) -> bool {
    let regex_pattern = pattern.replace('%', ".*").replace('_', ".");

    regex::Regex::new(&format!("^{}$", regex_pattern))
        .map(|re| re.is_match(value))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn data(pairs: &[(&str, Value)]) -> HashMap<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    fn eq(field: &str, value: &str) -> FilterClause {
        FilterClause::Single(Filter {
            field: field.to_string(),
            op: FilterOp::Equals(value.to_string()),
        })
    }

    fn neq(field: &str, value: &str) -> FilterClause {
        FilterClause::Single(Filter {
            field: field.to_string(),
            op: FilterOp::NotEquals(value.to_string()),
        })
    }

    // ── Empty constraints ───────────────────────────────────────────

    #[test]
    fn empty_constraints_always_match() {
        assert!(matches_constraints(&HashMap::new(), &[]));
        assert!(matches_constraints(&data(&[("x", json!("y"))]), &[]));
    }

    // ── Equals ──────────────────────────────────────────────────────

    #[test]
    fn equals_string_match() {
        let d = data(&[("owner", json!("user1"))]);
        assert!(matches_constraints(&d, &[eq("owner", "user1")]));
    }

    #[test]
    fn equals_string_no_match() {
        let d = data(&[("owner", json!("user2"))]);
        assert!(!matches_constraints(&d, &[eq("owner", "user1")]));
    }

    #[test]
    fn equals_number() {
        let d = data(&[("count", json!(42))]);
        assert!(matches_constraints(&d, &[eq("count", "42")]));
    }

    #[test]
    fn equals_bool_true() {
        let d = data(&[("active", json!(true))]);
        assert!(matches_constraints(&d, &[eq("active", "1")]));
    }

    #[test]
    fn equals_bool_false() {
        let d = data(&[("active", json!(false))]);
        assert!(matches_constraints(&d, &[eq("active", "0")]));
    }

    // ── NotEquals ───────────────────────────────────────────────────

    #[test]
    fn not_equals_match() {
        let d = data(&[("status", json!("draft"))]);
        assert!(matches_constraints(&d, &[neq("status", "published")]));
    }

    #[test]
    fn not_equals_no_match() {
        let d = data(&[("status", json!("published"))]);
        assert!(!matches_constraints(&d, &[neq("status", "published")]));
    }

    // ── Missing field (fail-closed) ─────────────────────────────────

    #[test]
    fn missing_field_fails_closed() {
        let d = data(&[("title", json!("hello"))]);
        assert!(!matches_constraints(&d, &[eq("owner", "user1")]));
    }

    #[test]
    fn missing_field_not_exists_matches() {
        let d = data(&[("title", json!("hello"))]);
        assert!(matches_constraints(
            &d,
            &[FilterClause::Single(Filter {
                field: "deleted".to_string(),
                op: FilterOp::NotExists,
            })]
        ));
    }

    // ── Exists / NotExists ──────────────────────────────────────────

    #[test]
    fn exists_with_field_present() {
        let d = data(&[("email", json!("a@b.com"))]);
        assert!(matches_constraints(
            &d,
            &[FilterClause::Single(Filter {
                field: "email".to_string(),
                op: FilterOp::Exists,
            })]
        ));
    }

    #[test]
    fn exists_with_field_absent() {
        let d = data(&[("name", json!("test"))]);
        assert!(!matches_constraints(
            &d,
            &[FilterClause::Single(Filter {
                field: "email".to_string(),
                op: FilterOp::Exists,
            })]
        ));
    }

    // ── Contains ────────────────────────────────────────────────────

    #[test]
    fn contains_match() {
        let d = data(&[("title", json!("hello world"))]);
        assert!(matches_constraints(
            &d,
            &[FilterClause::Single(Filter {
                field: "title".to_string(),
                op: FilterOp::Contains("world".to_string()),
            })]
        ));
    }

    #[test]
    fn contains_no_match() {
        let d = data(&[("title", json!("hello"))]);
        assert!(!matches_constraints(
            &d,
            &[FilterClause::Single(Filter {
                field: "title".to_string(),
                op: FilterOp::Contains("world".to_string()),
            })]
        ));
    }

    // ── Comparison operators ────────────────────────────────────────

    #[test]
    fn greater_than_numeric() {
        let d = data(&[("age", json!(25))]);
        assert!(matches_constraints(
            &d,
            &[FilterClause::Single(Filter {
                field: "age".to_string(),
                op: FilterOp::GreaterThan("18".to_string()),
            })]
        ));
        assert!(!matches_constraints(
            &d,
            &[FilterClause::Single(Filter {
                field: "age".to_string(),
                op: FilterOp::GreaterThan("30".to_string()),
            })]
        ));
    }

    #[test]
    fn less_than_or_equal() {
        let d = data(&[("score", json!(100))]);
        assert!(matches_constraints(
            &d,
            &[FilterClause::Single(Filter {
                field: "score".to_string(),
                op: FilterOp::LessThanOrEqual("100".to_string()),
            })]
        ));
        assert!(!matches_constraints(
            &d,
            &[FilterClause::Single(Filter {
                field: "score".to_string(),
                op: FilterOp::LessThanOrEqual("99".to_string()),
            })]
        ));
    }

    // ── In / NotIn ──────────────────────────────────────────────────

    #[test]
    fn in_values_match() {
        let d = data(&[("role", json!("admin"))]);
        assert!(matches_constraints(
            &d,
            &[FilterClause::Single(Filter {
                field: "role".to_string(),
                op: FilterOp::In(vec!["admin".to_string(), "editor".to_string()]),
            })]
        ));
    }

    #[test]
    fn in_values_no_match() {
        let d = data(&[("role", json!("viewer"))]);
        assert!(!matches_constraints(
            &d,
            &[FilterClause::Single(Filter {
                field: "role".to_string(),
                op: FilterOp::In(vec!["admin".to_string(), "editor".to_string()]),
            })]
        ));
    }

    #[test]
    fn not_in_values() {
        let d = data(&[("status", json!("active"))]);
        assert!(matches_constraints(
            &d,
            &[FilterClause::Single(Filter {
                field: "status".to_string(),
                op: FilterOp::NotIn(vec!["deleted".to_string(), "archived".to_string()]),
            })]
        ));
    }

    // ── Multiple filters (AND) ──────────────────────────────────────

    #[test]
    fn multiple_filters_all_must_match() {
        let d = data(&[("owner", json!("user1")), ("status", json!("published"))]);
        assert!(matches_constraints(
            &d,
            &[eq("owner", "user1"), eq("status", "published")]
        ));
    }

    #[test]
    fn multiple_filters_one_fails() {
        let d = data(&[("owner", json!("user1")), ("status", json!("draft"))]);
        assert!(!matches_constraints(
            &d,
            &[eq("owner", "user1"), eq("status", "published")]
        ));
    }

    // ── Or groups ───────────────────────────────────────────────────

    #[test]
    fn or_group_first_matches() {
        let d = data(&[("role", json!("admin"))]);
        let clause = FilterClause::Or(vec![
            vec![Filter {
                field: "role".to_string(),
                op: FilterOp::Equals("admin".to_string()),
            }],
            vec![Filter {
                field: "role".to_string(),
                op: FilterOp::Equals("editor".to_string()),
            }],
        ]);
        assert!(matches_constraints(&d, &[clause]));
    }

    #[test]
    fn or_group_second_matches() {
        let d = data(&[("role", json!("editor"))]);
        let clause = FilterClause::Or(vec![
            vec![Filter {
                field: "role".to_string(),
                op: FilterOp::Equals("admin".to_string()),
            }],
            vec![Filter {
                field: "role".to_string(),
                op: FilterOp::Equals("editor".to_string()),
            }],
        ]);
        assert!(matches_constraints(&d, &[clause]));
    }

    #[test]
    fn or_group_none_match() {
        let d = data(&[("role", json!("viewer"))]);
        let clause = FilterClause::Or(vec![
            vec![Filter {
                field: "role".to_string(),
                op: FilterOp::Equals("admin".to_string()),
            }],
            vec![Filter {
                field: "role".to_string(),
                op: FilterOp::Equals("editor".to_string()),
            }],
        ]);
        assert!(!matches_constraints(&d, &[clause]));
    }

    // ── Null values ─────────────────────────────────────────────────

    /// Null JSON values compare as empty string. This mirrors SQLite behavior
    /// where NULL text columns coerce to "" in comparisons, and matches how
    /// event data arrives from the DB layer (serde_json maps SQL NULL → Value::Null,
    /// and `value_to_string` normalizes it to ""). Without this, a filter like
    /// `status != "deleted"` would fail-closed on documents where status is null,
    /// incorrectly blocking access.
    #[test]
    fn null_value_equals_empty_string() {
        let d = data(&[("field", Value::Null)]);
        assert!(matches_constraints(&d, &[eq("field", "")]));
    }

    #[test]
    fn null_value_not_equals_nonempty() {
        let d = data(&[("field", Value::Null)]);
        assert!(!matches_constraints(&d, &[eq("field", "something")]));
    }
}
