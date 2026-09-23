//! In-memory filter evaluation for `FilterClause` against document data.
//!
//! Used where row-level access constraints must be enforced without a DB query:
//! - **Event streams** (SSE, gRPC `Subscribe`) — gate each event by the
//!   subscriber's read constraints, judged against the stored row the event
//!   carries for that purpose (`EventGateSnapshot`).
//! - **Relationship/join population** — gate each embedded target row by the
//!   target collection's view constraints, matched against the raw cached
//!   document (see `populate::helpers::target_row_visible`).
//! - **Draft reads** — gate a draft version snapshot, which bypasses the SQL
//!   `WHERE`, by the view's row constraint (see `db::ops::find_by_id_full`).
//!
//! A caller holding a [`Document`](crate::core::Document) judges it through
//! [`matches_document`] (or snapshots it with [`constraint_row`]), never by
//! passing `doc.fields` alone: a document keeps `id` and the timestamps outside
//! its field map, and a constraint naming them (`{ id = user.id }`) must see
//! the row's columns as SQL sees them.
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
//! guidance) and the two paths agree. A scalar has-many list is matched element
//! by element with the SQL builder's list reading (see `super::elements`), and
//! a path into array or blocks rows asks for some row that satisfies it, as the
//! SQL subquery does (see `rows`).
//!
//! A field the data holds as `null` is a NULL column. A top-level field the
//! data does not carry at all is **unknown** — a partial or empty document
//! says nothing about it — so no operator matches it, negative ones included
//! (fail-closed). Inside a row's JSON a
//! missing key stays NULL, as SQL's `json_extract` reads it.
//!
//! There is intentionally no operator-rejection here: enforcement is by
//! convention + documentation, not by narrowing what a Lua hook may return.

mod document;
mod like;
mod lists;
mod presence;
mod rows;
mod schema;

use std::cmp::Ordering;

use serde_json::Value;

pub use self::document::{constraint_row, matches_document};

use self::{
    like::matches_like,
    lists::{list_elements, matches_list, reference_ids},
    presence::{lookup, matches_null},
    rows::matches_row_path,
    schema::{Leaf, Schema},
};
use crate::{
    core::{
        DocumentFields, FieldDefinition, FieldType, canonical_operand, checkbox_value,
        flatten_group_fields, parse_bool, parse_number,
    },
    db::{
        Filter, FilterClause, FilterOp,
        query::helpers::{ListPlace, normalize_date_value},
    },
};

/// Evaluate filter clauses against in-memory document data, coercing comparisons
/// by each constrained field's type so the result mirrors the SQL `WHERE` path.
///
/// `fields` is the field list of the collection the `data` belongs to. It is
/// used to resolve a constraint's field type (Checkbox, Number, …) so a value
/// like `{ active = { not_equals = true } }` is compared the same way SQL's
/// `coerce_filter_value` binds it — without this the matcher does a blind string
/// compare (`"1" != "true"`) and fails OPEN versus SQL on Checkbox/Number.
///
/// Returns `true` if all clauses match (AND semantics, same as SQL WHERE).
/// A field the data holds as `null` reads as a SQL NULL: only `not_exists`
/// matches it. A top-level field the data does not carry at all cannot be
/// judged, so no operator matches it — not even a negative one (fail-closed):
/// a partial or empty payload never satisfies a constraint on a field it lacks.
/// Returns `true` for empty constraints (no filters = no restrictions).
#[must_use]
pub fn matches_constraints_typed(
    data: &DocumentFields,
    constraints: &[FilterClause],
    fields: &[FieldDefinition],
) -> bool {
    if constraints.is_empty() {
        return true;
    }

    // Constraints name a group's sub-field by its flat path (`seo__owner`),
    // while documents carry the group nested.
    let flat = flatten_group_fields(data, fields);

    matches_flat(&flat, constraints, fields)
}

/// Evaluate `constraints` (AND) against data whose groups are already flat.
fn matches_flat(
    flat: &DocumentFields,
    constraints: &[FilterClause],
    fields: &[FieldDefinition],
) -> bool {
    let schema = Schema::new(fields);

    constraints
        .iter()
        .all(|clause| matches_clause(flat, clause, &schema))
}

/// `matches_constraints_typed` with no field-type information — a blind string
/// comparison. Test-only: every production caller passes the real field list so
/// Checkbox/Number constraints match SQL.
#[cfg(test)]
pub(crate) fn matches_constraints(data: &DocumentFields, constraints: &[FilterClause]) -> bool {
    matches_constraints_typed(data, constraints, &[])
}

/// Evaluate one [`FilterClause`] tree node against document data, recursing
/// through `And`/`Or`. An empty `And` matches (`all` over none is `true`); an
/// empty `Or` does not (`any` over none is `false`) — the same identities the
/// SQL builder renders as `1=1` / `1=0`.
fn matches_clause(data: &DocumentFields, clause: &FilterClause, schema: &Schema<'_>) -> bool {
    match clause {
        FilterClause::Single(filter) => matches_filter(data, filter, schema),
        FilterClause::And(subs) => subs.iter().all(|c| matches_clause(data, c, schema)),
        FilterClause::Or(subs) => subs.iter().any(|c| matches_clause(data, c, schema)),
    }
}

/// Evaluate a single filter against document data.
fn matches_filter(data: &DocumentFields, filter: &Filter, schema: &Schema<'_>) -> bool {
    if let Some(matched) = matches_row_path(data, filter, schema.fields) {
        return matched;
    }

    let leaf = schema.types.get(&filter.field);

    // A path the payload does not carry cannot be judged: no operator — not
    // even a negative one — matches it (fail-closed).
    let root = match leaf {
        Some(Leaf::References { root, .. }) => root.as_str(),
        _ => filter.field.as_str(),
    };
    let Some(value) = lookup(data, root) else {
        return false;
    };

    matches_leaf(value, &filter.op, leaf)
}

/// Evaluate an operator against the value a document carries for a leaf —
/// a list element by element, a single value as SQL compares its column.
fn matches_leaf(value: &Value, op: &FilterOp, leaf: Option<&Leaf>) -> bool {
    let field_type = match leaf {
        Some(Leaf::List(field_type)) => {
            let elements = list_elements(Some(value), field_type, ListPlace::Column);

            return matches_list(&elements, op, Some(field_type));
        }
        Some(Leaf::References { polymorphic, .. }) => {
            let ids = reference_ids(Some(value), *polymorphic);

            return matches_list(&ids, op, Some(&FieldType::Text));
        }
        Some(Leaf::Value(field_type)) => Some(field_type),
        None => None,
    };

    // A present JSON `null` is a NULL column: SQL three-valued logic excludes
    // NULL rows from every comparison except `IS NULL`, so it must NOT satisfy
    // `Equals`/`NotEquals`/`In`/`NotIn`/`Exists` (and must satisfy `NotExists`).
    // Without this, `value_to_string` would coerce `null` to `""` and
    // `NotEquals`/`NotIn` would match (fail-open) while SQL excludes the row — a
    // leak on the populate/event/snapshot paths.
    if value.is_null() {
        return matches_null(op);
    }

    matches_value(value, op, field_type)
}

/// Evaluate an operator against one present, non-null value.
fn matches_value(value: &Value, op: &FilterOp, ft: Option<&FieldType>) -> bool {
    let value_str = value_to_string(value);
    // Operands in the form the field's values are stored in, as SQL binds them:
    // otherwise an email typed with capitals fails `equals` and passes
    // `not_equals` here while SQL decides the reverse.
    let operand = |raw: &str| canonical_operand(ft, raw).into_owned();

    match op {
        // Equality/membership are coerced by field type so Checkbox/Number agree
        // with SQL's `coerce_filter_value` instead of a blind string compare.
        FilterOp::Equals(expected) => typed_eq(value, &operand(expected), ft),
        FilterOp::NotEquals(expected) => !typed_eq(value, &operand(expected), ft),
        FilterOp::In(values) => values.iter().any(|e| typed_eq(value, &operand(e), ft)),
        FilterOp::NotIn(values) => !values.iter().any(|e| typed_eq(value, &operand(e), ft)),
        FilterOp::Contains(needle) => value_str
            .to_ascii_lowercase()
            .contains(&operand(needle).to_ascii_lowercase()),
        FilterOp::Like(pattern) => matches_like(&value_str, &operand(pattern)),
        FilterOp::GreaterThan(expected) => {
            order_is(value, &operand(expected), Ordering::Greater, false)
        }
        FilterOp::LessThan(expected) => order_is(value, &operand(expected), Ordering::Less, false),
        FilterOp::GreaterThanOrEqual(expected) => {
            order_is(value, &operand(expected), Ordering::Greater, true)
        }
        FilterOp::LessThanOrEqual(expected) => {
            order_is(value, &operand(expected), Ordering::Less, true)
        }
        FilterOp::Exists => true, // the value is present (checked by the caller)
        FilterOp::NotExists => false, // present, but the op says it shouldn't be
    }
}

/// Type-aware equality between a stored JSON value and a constraint string,
/// mirroring SQL's per-column coercion (`coerce_filter_value`) so the in-memory
/// path agrees with SQL. Falls back to string comparison for non-typed fields.
fn typed_eq(stored: &Value, expected: &str, ft: Option<&FieldType>) -> bool {
    match ft {
        Some(FieldType::Checkbox) => match bool_from_str(expected) {
            Some(b) => checkbox_value(stored) == Some(b),
            // Unrecognized boolean spelling: SQL falls back to a text compare.
            None => value_to_string(stored) == *expected,
        },
        Some(FieldType::Number) => {
            // Mirror SQL `coerce_filter_value`: only a *finite* parse binds as a
            // number; `inf`/`NaN` fall through to a text compare on both sides.
            let parsed = parse_number(expected).filter(|f| f.is_finite());
            match (num_repr(stored), parsed) {
                (Some(a), Some(b)) => a == b,
                // Non-numeric on either side: SQL falls back to a text compare.
                _ => value_to_string(stored) == *expected,
            }
        }
        // SQL binds a Date comparand through `normalize_date_value` (and the
        // stored column was normalized at write time), so normalize both sides
        // here too — otherwise `2026-01-15` vs `2026-01-15T00:00:00.000Z` would
        // diverge (fail-open for `NotEquals`/`NotIn`).
        Some(FieldType::Date) => {
            normalize_date_value(&value_to_string(stored)) == normalize_date_value(expected)
        }
        _ => value_to_string(stored) == *expected,
    }
}

/// Parse a constraint's boolean spelling — the shared `core::parse_bool` token
/// set (`1/true/yes/on` → true, `0/false/no/off` → false), so the in-memory
/// filter, the SQL filter, and the write-coerce edge agree exactly. The *stored*
/// value is read with the wider `core::checkbox_value` rule, which also accepts
/// the number a column holds.
fn bool_from_str(s: &str) -> Option<bool> {
    parse_bool(s)
}

/// Numeric value of a stored Number field (`Number` or numeric string).
fn num_repr(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => parse_number(s),
        _ => None,
    }
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
        Value::Number(n) => n.as_f64()?.partial_cmp(&parse_number(expected)?),
        Value::String(s) => Some(s.as_str().cmp(expected)),
        _ => None,
    }
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

    /// Regression: SQL compares an email or text operand in its stored form
    /// while this matcher compared it as typed, so an access rule's
    /// `not_equals` on an address typed with capitals let the row through on
    /// live events and population while SQL excluded it.
    #[test]
    fn email_and_text_operands_compare_in_their_stored_form() {
        let fields = vec![
            FieldDefinition::builder("email", FieldType::Email).build(),
            FieldDefinition::builder("name", FieldType::Text).build(),
        ];
        let d = data(&[("email", json!("bob@x.com")), ("name", json!("Caf\u{e9}"))]);

        assert!(!matches_constraints_typed(
            &d,
            &[neq("email", "Bob@X.com")],
            &fields
        ));
        assert!(matches_constraints_typed(
            &d,
            &[eq("email", " Bob@X.com")],
            &fields
        ));
        assert!(matches_constraints_typed(
            &d,
            &[eq("name", "Cafe\u{301}")],
            &fields
        ));
        assert!(!matches_constraints_typed(
            &d,
            &[neq("name", "Cafe\u{301}")],
            &fields
        ));
    }

    /// Regression: the matcher read constraint paths at the top level only, so a
    /// rule on a group's sub-field (`seo__owner`) never saw a document carrying
    /// the group nested — `equals` refused the row, `not_exists` let it through.
    #[test]
    fn group_sub_field_constraints_match_nested_groups() {
        let fields = vec![
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("owner", FieldType::Text).build(),
                ])
                .build(),
        ];
        let d = data(&[("seo", json!({ "owner": "u1" }))]);
        let clause = |op: FilterOp| {
            FilterClause::Single(Filter {
                field: "seo__owner".to_string(),
                op,
            })
        };

        assert!(matches_constraints_typed(
            &d,
            &[clause(FilterOp::Equals("u1".to_string()))],
            &fields
        ));
        assert!(!matches_constraints_typed(
            &d,
            &[clause(FilterOp::NotExists)],
            &fields
        ));
    }

    /// Regression: ordered operators compared the operand as typed, so a text
    /// bound typed with a combining accent sorted apart from the same stored
    /// value.
    #[test]
    fn ordered_operands_compare_in_their_stored_form() {
        let fields = vec![FieldDefinition::builder("name", FieldType::Text).build()];
        let d = data(&[("name", json!("Caf\u{e9}"))]);
        let clause = |op: FilterOp| {
            FilterClause::Single(Filter {
                field: "name".to_string(),
                op,
            })
        };

        assert!(matches_constraints_typed(
            &d,
            &[clause(FilterOp::LessThanOrEqual("Cafe\u{301}".to_string()))],
            &fields
        ));
        assert!(matches_constraints_typed(
            &d,
            &[clause(FilterOp::GreaterThanOrEqual(
                "Cafe\u{301}".to_string()
            ))],
            &fields
        ));
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

    // ── Contains ────────────────────────────────────────────────────

    /// `contains` folds ASCII case like SQL `LIKE` / `ILIKE`, so the in-memory
    /// check agrees with the database.
    #[test]
    fn contains_is_ascii_case_insensitive() {
        let d = data(&[("title", json!("Hello World"))]);
        assert!(matches_constraints(
            &d,
            &[FilterClause::Single(Filter {
                field: "title".to_string(),
                op: FilterOp::Contains("WORLD".to_string()),
            })]
        ));
    }

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

    /// The leak-critical direction: a row whose value IS in the `NotIn` set must
    /// be excluded. An always-true `NotIn` (the classic negation-operator bug)
    /// would let a row-scoped subscriber see rows outside their scope.
    #[test]
    fn not_in_values_excludes_member() {
        let d = data(&[("status", json!("archived"))]);
        assert!(!matches_constraints(
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

    // ── Field-type-aware equality (Checkbox / Number) ───────────────

    fn typed_single(field: &str, op: FilterOp) -> FilterClause {
        FilterClause::Single(Filter {
            field: field.to_string(),
            op,
        })
    }

    /// Regression for the fail-open Checkbox leak: `{ active = { not_equals =
    /// true } }` (operator-table bool → `"true"`) must NOT match a Checkbox
    /// stored as `1`, matching SQL (which coerces `"true"` → `1` and excludes
    /// the row). The old blind string compare (`"1" != "true"`) returned true.
    #[test]
    fn checkbox_not_equals_true_matches_sql() {
        let fields = vec![FieldDefinition::builder("active", FieldType::Checkbox).build()];
        let d = data(&[("active", json!(1))]);

        let neq = typed_single("active", FilterOp::NotEquals("true".into()));
        assert!(!matches_constraints_typed(&d, from_ref(&neq), &fields));

        let eq = typed_single("active", FilterOp::Equals("true".into()));
        assert!(matches_constraints_typed(&d, from_ref(&eq), &fields));
    }

    /// Checkbox stored as `Bool(true)` or `1` both coerce, and every boolean
    /// spelling (`true`/`1`/`yes`/`on`) compares equal.
    #[test]
    fn checkbox_bool_and_int_representations_agree() {
        let fields = vec![FieldDefinition::builder("active", FieldType::Checkbox).build()];
        for stored in [json!(true), json!(1)] {
            let d = data(&[("active", stored.clone())]);
            for spelling in ["true", "1", "yes", "on"] {
                let eq = typed_single("active", FilterOp::Equals(spelling.into()));
                assert!(
                    matches_constraints_typed(&d, from_ref(&eq), &fields),
                    "active == {spelling} (stored {stored})"
                );
            }
        }
    }

    /// A stored Checkbox reads through the one checkbox rule: any non-zero
    /// number is checked, and a numeric string means what the number means.
    /// (`NaN` cannot be a JSON number — `serde_json` refuses to build one — so
    /// the rule's `NaN`-is-unchecked arm is only reachable through a string.)
    #[test]
    fn checkbox_stored_value_follows_the_one_checkbox_rule() {
        let fields = vec![FieldDefinition::builder("active", FieldType::Checkbox).build()];

        for stored in [json!(2), json!("2"), json!(0.5), json!("on")] {
            let d = data(&[("active", stored.clone())]);
            let eq = typed_single("active", FilterOp::Equals("true".into()));
            assert!(
                matches_constraints_typed(&d, from_ref(&eq), &fields),
                "stored {stored} must read as checked"
            );
        }

        for stored in [json!(0), json!("0"), json!(0.0), json!("NaN")] {
            let d = data(&[("active", stored.clone())]);
            let eq = typed_single("active", FilterOp::Equals("false".into()));
            assert!(
                matches_constraints_typed(&d, from_ref(&eq), &fields),
                "stored {stored} must read as unchecked"
            );
        }
    }

    /// A number spelled with surrounding whitespace compares as the number it
    /// spells, on both the equality and the ordered path — the shared
    /// `core::parse_number` reading, so a constraint isn't silently demoted to
    /// a text compare by a stray space.
    #[test]
    fn a_padded_number_constraint_compares_numerically() {
        let fields = vec![FieldDefinition::builder("score", FieldType::Number).build()];
        let d = data(&[("score", json!(5))]);

        let eq = typed_single("score", FilterOp::Equals(" 5".into()));
        assert!(matches_constraints_typed(&d, from_ref(&eq), &fields));

        let gt = typed_single("score", FilterOp::GreaterThan(" 4 ".into()));
        assert!(matches_constraints_typed(&d, from_ref(&gt), &fields));

        let lt = typed_single("score", FilterOp::LessThan(" 4 ".into()));
        assert!(!matches_constraints_typed(&d, from_ref(&lt), &fields));
    }

    /// Regression: Date constraints normalize both sides like SQL, so a stored
    /// `…T09:00:00.000Z` equals a constraint `…T09:00:00Z`. The old raw string
    /// compare made `NotEquals` fail OPEN here (the strings differ).
    #[test]
    fn date_constraint_normalizes_both_sides_like_sql() {
        let fields = vec![FieldDefinition::builder("published_at", FieldType::Date).build()];
        let d = data(&[("published_at", json!("2026-01-15T09:00:00.000Z"))]);

        let eq = typed_single(
            "published_at",
            FilterOp::Equals("2026-01-15T09:00:00Z".into()),
        );
        assert!(matches_constraints_typed(&d, from_ref(&eq), &fields));

        let neq = typed_single(
            "published_at",
            FilterOp::NotEquals("2026-01-15T09:00:00Z".into()),
        );
        assert!(!matches_constraints_typed(&d, from_ref(&neq), &fields));
    }

    /// Number constraints compare numerically (`3` == `3.0`), not as strings.
    #[test]
    fn number_constraint_compares_numerically() {
        let fields = vec![FieldDefinition::builder("score", FieldType::Number).build()];
        let d = data(&[("score", json!(3.0))]);

        let eq = typed_single("score", FilterOp::Equals("3".into()));
        assert!(matches_constraints_typed(&d, from_ref(&eq), &fields));

        let neq = typed_single("score", FilterOp::NotEquals("3".into()));
        assert!(!matches_constraints_typed(&d, from_ref(&neq), &fields));
    }

    /// SQL `coerce_filter_value` only binds a *finite* parse as a number; `inf`/
    /// `NaN` fall through to a text compare. The in-memory matcher must do the
    /// same so the two paths can't diverge (`NotEquals "inf"` must not fail-open).
    #[test]
    fn non_finite_number_constraint_matches_sql_text_fallback() {
        let fields = vec![FieldDefinition::builder("score", FieldType::Number).build()];
        let d = data(&[("score", json!(3.0))]);

        // `inf`/`NaN` are not numeric comparands: a finite stored value never
        // equals them, and (the divergence guard) NotEquals must hold true.
        for spelling in ["inf", "Infinity", "NaN", "-inf"] {
            let eq = typed_single("score", FilterOp::Equals(spelling.into()));
            assert!(
                !matches_constraints_typed(&d, from_ref(&eq), &fields),
                "Equals {spelling} must not match a finite number"
            );

            let neq = typed_single("score", FilterOp::NotEquals(spelling.into()));
            assert!(
                matches_constraints_typed(&d, from_ref(&neq), &fields),
                "NotEquals {spelling} must hold (text fallback, like SQL) — not fail-open"
            );
        }
    }
}
