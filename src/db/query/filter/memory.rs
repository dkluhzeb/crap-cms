//! In-memory filter evaluation for `FilterClause` against document data.
//!
//! Used where row-level access constraints must be enforced without a DB query:
//! - **Event streams** (SSE, gRPC `Subscribe`) — gate each event by the
//!   subscriber's read constraints.
//! - **Relationship/join population** — gate each embedded target row by the
//!   target collection's view constraints, matched against the raw cached
//!   document (see `populate::helpers::target_row_visible`).
//!
//! It evaluates the same `FilterClause` types that `Find` compiles to SQL WHERE
//! clauses, and is **exact** for the shapes access rules should use — equality
//! and membership (`Equals`/`NotEquals`/`In`/`NotIn`), presence
//! (`Exists`/`NotExists`), and `Like` — whose wildcards/literals are translated
//! faithfully and folded ASCII-case-insensitively to match `SQLite` `LIKE` /
//! Postgres `ILIKE`. *Ordered* comparisons (`>`/`<`/`>=`/`<=`) choose numeric vs
//! lexicographic by the field's JSON type — a numeric field compares
//! numerically, a text field lexicographically — mirroring SQL column affinity
//! for realistic data (only `SQLite`'s exotic mixed-type affinity edges, which
//! aren't a sensible access constraint, are left unreplicated). Keep access
//! rules to equality/membership/presence on your own fields (the documented
//! guidance) and the two paths agree. There is intentionally no
//! operator-rejection here: enforcement is by convention + documentation, not by
//! narrowing what a Lua hook may return.

use std::cmp::Ordering;

use serde_json::Value;

use crate::core::DocumentFields;
use crate::db::{Filter, FilterClause, FilterOp};

/// Evaluate filter clauses against in-memory document data.
///
/// Returns `true` if all clauses match (AND semantics, same as SQL WHERE).
/// Returns `false` (fail-closed) if a referenced field is missing from data.
/// Returns `true` for empty constraints (no filters = no restrictions).
#[must_use]
pub fn matches_constraints(data: &DocumentFields, constraints: &[FilterClause]) -> bool {
    if constraints.is_empty() {
        return true;
    }

    constraints
        .iter()
        .all(|clause| matches_clause(data, clause))
}

/// Evaluate one [`FilterClause`] tree node against document data, recursing
/// through `And`/`Or`. An empty `And` matches (`all` over none is `true`); an
/// empty `Or` does not (`any` over none is `false`) — the same identities the
/// SQL builder renders as `1=1` / `1=0`.
fn matches_clause(data: &DocumentFields, clause: &FilterClause) -> bool {
    match clause {
        FilterClause::Single(filter) => matches_filter(data, filter),
        FilterClause::And(subs) => subs.iter().all(|c| matches_clause(data, c)),
        FilterClause::Or(subs) => subs.iter().any(|c| matches_clause(data, c)),
    }
}

/// Evaluate a single filter against document data.
fn matches_filter(data: &DocumentFields, filter: &Filter) -> bool {
    let Some(value) = data.get(&filter.field) else {
        return matches_missing_field(&filter.op);
    };

    let value_str = value_to_string(value);

    match &filter.op {
        FilterOp::Equals(expected) => value_str == *expected,
        FilterOp::NotEquals(expected) => value_str != *expected,
        FilterOp::Contains(needle) => value_str.contains(needle.as_str()),
        FilterOp::Like(pattern) => matches_like(&value_str, pattern),
        FilterOp::GreaterThan(expected) => order_is(value, expected, Ordering::Greater, false),
        FilterOp::LessThan(expected) => order_is(value, expected, Ordering::Less, false),
        FilterOp::GreaterThanOrEqual(expected) => {
            order_is(value, expected, Ordering::Greater, true)
        }
        FilterOp::LessThanOrEqual(expected) => order_is(value, expected, Ordering::Less, true),
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
/// Whether the field compares to `expected` with the wanted ordering. `allow_eq`
/// folds in equality (for `>=`/`<=`). Returns `false` (fail-closed) when the
/// operands are not ordered-comparable.
fn order_is(value: &Value, expected: &str, want: Ordering, allow_eq: bool) -> bool {
    match compare_typed(value, expected) {
        Some(ord) => ord == want || (allow_eq && ord == Ordering::Equal),
        None => false,
    }
}

/// Order a field value against a string comparand, choosing the comparison by
/// the field's JSON type so the in-memory path mirrors SQL column affinity: a
/// **numeric** field compares numerically (matching a numeric column), a
/// **text** field lexicographically (matching a text column). This avoids the
/// "text field holding `\"100\"` vs `\"50\"`" divergence a blanket numeric
/// coercion would cause. Non-scalar / incomparable operands yield `None` (the
/// caller fails closed). Exotic mixed-type SQL affinity edges are not replicated
/// — ordered comparisons aren't a sensible access constraint on text anyway.
fn compare_typed(value: &Value, expected: &str) -> Option<Ordering> {
    match value {
        Value::Number(n) => n.as_f64()?.partial_cmp(&expected.parse::<f64>().ok()?),
        Value::String(s) => Some(s.as_str().cmp(expected)),
        _ => None,
    }
}

/// Simple LIKE pattern matching (SQL-style: % = any chars, _ = single char).
fn matches_like(value: &str, pattern: &str) -> bool {
    // Match the SQL backends exactly so the in-memory and SQL paths agree:
    // - Escape regex metacharacters in the literal portion FIRST, so a `.`/`(`/`[`
    //   in the pattern matches itself (as SQL `LIKE` does); without this the
    //   in-memory path would over-match (fail-open).
    // - Translate the SQL wildcards `%` → `.*`, `_` → `.` (one char), which
    //   `regex::escape` leaves untouched.
    // - Fold ASCII case on both sides: SQLite `LIKE` is ASCII-case-insensitive
    //   by default and Postgres uses `ILIKE`, so case-insensitive is the expected
    //   behavior. ASCII-only folding (not Unicode) tracks SQLite precisely and
    //   stays at most stricter than Postgres `ILIKE` for non-ASCII (fail-closed).
    let regex_pattern = regex::escape(pattern)
        .replace('%', ".*")
        .replace('_', ".")
        .to_ascii_lowercase();

    regex::Regex::new(&format!("^{regex_pattern}$"))
        .is_ok_and(|re| re.is_match(&value.to_ascii_lowercase()))
}

#[cfg(test)]
mod tests {
    use std::slice::from_ref;

    use serde_json::json;

    use super::*;

    fn data(pairs: &[(&str, Value)]) -> DocumentFields {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    /// `Like` must treat regex metacharacters in the pattern as literals (SQL
    /// semantics), not as regex — otherwise in-memory matching over-matches
    /// relative to SQL, a fail-open on the access-gating path.
    #[test]
    fn matches_like_treats_metacharacters_literally() {
        // `.` is a literal dot, not "any char": "a.c" matches, "axc" must not.
        assert!(matches_like("a.c", "a.c"));
        assert!(!matches_like("axc", "a.c"));

        // SQL wildcards still work: `%` = any run, `_` = any single char.
        assert!(matches_like("axc", "a%c"));
        assert!(matches_like("axc", "a_c"));

        // Regex group/alternation metachars are literal too.
        assert!(matches_like("a(b)c", "a(b)c"));
        assert!(!matches_like("ab", "a|b"));
    }

    /// `Like` is ASCII-case-insensitive on both sides, matching `SQLite` `LIKE` /
    /// Postgres `ILIKE`, so the in-memory and SQL paths agree.
    #[test]
    fn matches_like_is_ascii_case_insensitive() {
        assert!(matches_like("Hello", "hello"));
        assert!(matches_like("hello", "HELLO"));
        assert!(matches_like("ALICE@X.COM", "alice@%"));
        assert!(matches_like("Bob", "b_b"));
        // Non-letters and structure still matter.
        assert!(!matches_like("hellp", "hello"));
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
        assert!(matches_constraints(&DocumentFields::new(), &[]));
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

    /// Ordered comparison picks numeric vs lexicographic by the field's JSON
    /// type, mirroring SQL column affinity: a TEXT field holding numeric-looking
    /// strings compares lexicographically (matching a SQL text column), NOT
    /// numerically — which a blanket numeric coercion would get wrong.
    #[test]
    fn ordering_respects_field_type_like_sql_affinity() {
        let text = data(&[("code", json!("100"))]);
        // Lexicographic: "100" < "50" (`'1' < '5'`), so `> "50"` is false.
        assert!(!matches_constraints(
            &text,
            &[FilterClause::Single(Filter {
                field: "code".to_string(),
                op: FilterOp::GreaterThan("50".to_string()),
            })]
        ));
        // …but "100" > "0" lexicographically.
        assert!(matches_constraints(
            &text,
            &[FilterClause::Single(Filter {
                field: "code".to_string(),
                op: FilterOp::GreaterThan("0".to_string()),
            })]
        ));
        // A genuine numeric field still compares numerically (100 > 50).
        let num = data(&[("n", json!(100))]);
        assert!(matches_constraints(
            &num,
            &[FilterClause::Single(Filter {
                field: "n".to_string(),
                op: FilterOp::GreaterThan("50".to_string()),
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
        let clause = FilterClause::or_groups(vec![
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
        let clause = FilterClause::or_groups(vec![
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
        let clause = FilterClause::or_groups(vec![
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

    /// Recursive evaluation of a nested tree the flat type could not hold:
    /// `(a AND b) OR (c AND (d OR e))`. Mirrors the SQL builder's nesting test
    /// so the two evaluators stay in agreement.
    #[test]
    fn nested_and_or_evaluates() {
        let clause = FilterClause::Or(vec![
            FilterClause::And(vec![eq("a", "1"), eq("b", "2")]),
            FilterClause::And(vec![
                eq("c", "3"),
                FilterClause::Or(vec![eq("d", "4"), eq("e", "5")]),
            ]),
        ]);

        // Second arm matches: c = 3 and (the OR's) e = 5.
        let hit = data(&[("c", json!("3")), ("e", json!("5"))]);
        assert!(matches_constraints(&hit, from_ref(&clause)));

        // c matches but neither d nor e does → the AND arm fails, no match.
        let miss = data(&[("c", json!("3")), ("e", json!("nope"))]);
        assert!(!matches_constraints(&miss, from_ref(&clause)));
    }

    // ── Null values ─────────────────────────────────────────────────

    /// Null JSON values compare as empty string. This mirrors `SQLite` behavior
    /// where NULL text columns coerce to "" in comparisons, and matches how
    /// event data arrives from the DB layer (`serde_json` maps SQL NULL → `Value::Null`,
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
