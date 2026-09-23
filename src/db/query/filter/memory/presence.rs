//! Whether the document carries a filtered field, for the in-memory
//! evaluator: a present `null` is a SQL NULL, a field the document does not
//! carry at all is unknown and matches no operator.

use serde_json::Value;

use crate::{core::DocumentFields, db::FilterOp};

/// The value a filter path reads from the (group-flattened) document: the key
/// itself, or a JSON `null` when an enclosing group is present as `null` — all
/// of that group's columns are then NULL. `None` when the document does not
/// carry the path at all: a partial payload says nothing about the value, so
/// the caller must not guess one.
pub(super) fn lookup<'a>(data: &'a DocumentFields, path: &str) -> Option<&'a Value> {
    if let Some(value) = data.get(path) {
        return Some(value);
    }

    path.match_indices("__")
        .filter_map(|(end, _)| path.get(..end))
        .find_map(|group| data.get(group).filter(|value| value.is_null()))
}

/// Evaluate an operator against a NULL value, as SQL does: only `NotExists`
/// (`IS NULL`) matches it.
pub(super) fn matches_null(op: &FilterOp) -> bool {
    matches!(op, FilterOp::NotExists)
}

#[cfg(test)]
mod tests {
    use std::slice::from_ref;

    use serde_json::json;

    use super::*;
    use crate::{
        core::{FieldDefinition, FieldType, RelationshipConfig},
        db::{
            Filter, FilterClause,
            query::filter::memory::{matches_constraints, matches_constraints_typed},
        },
    };

    fn data(pairs: &[(&str, Value)]) -> DocumentFields {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    fn eq(field: &str, value: &str) -> FilterClause {
        typed_single(field, FilterOp::Equals(value.to_string()))
    }

    fn neq(field: &str, value: &str) -> FilterClause {
        typed_single(field, FilterOp::NotEquals(value.to_string()))
    }

    fn typed_single(field: &str, op: FilterOp) -> FilterClause {
        FilterClause::Single(Filter {
            field: field.to_string(),
            op,
        })
    }

    // ── Missing field (fail-closed) ─────────────────────────────────

    #[test]
    fn missing_field_fails_closed() {
        let d = data(&[("title", json!("hello"))]);
        assert!(!matches_constraints(&d, &[eq("owner", "user1")]));
    }

    /// Regression: an absent field read like a NULL column, so `not_exists`
    /// matched a payload that simply did not carry the field (an empty
    /// metadata-only or delete event), letting a constrained subscriber
    /// receive it. Absent is unknown: no operator matches it.
    #[test]
    fn absent_field_matches_no_operator() {
        let d = data(&[("title", json!("hello"))]);
        let clause = |op: FilterOp| {
            FilterClause::Single(Filter {
                field: "deleted".to_string(),
                op,
            })
        };

        for op in [
            FilterOp::NotExists,
            FilterOp::NotEquals("x".into()),
            FilterOp::NotIn(vec!["x".into()]),
            FilterOp::Exists,
        ] {
            assert!(!matches_constraints(&d, from_ref(&clause(op))));
        }
    }

    /// Absent versus present-but-empty on a scalar has-many list and a has-many
    /// relationship: an empty or null list holds no element, so the negative
    /// operators match it as SQL does; a list the payload lacks matches nothing.
    #[test]
    fn absent_list_differs_from_present_empty_list() {
        let fields = vec![
            FieldDefinition::builder("tags", FieldType::Text)
                .has_many(true)
                .build(),
            FieldDefinition::builder("refs", FieldType::Relationship)
                .relationship(RelationshipConfig::new("things", true))
                .build(),
        ];
        let matches = |d: &DocumentFields, field: &str, op: FilterOp| {
            matches_constraints_typed(d, &[typed_single(field, op)], &fields)
        };
        let secret = || vec!["secret".to_string()];

        let absent = DocumentFields::new();
        assert!(!matches(&absent, "tags", FilterOp::NotIn(secret())));
        assert!(!matches(
            &absent,
            "tags",
            FilterOp::NotEquals("secret".into())
        ));
        assert!(!matches(&absent, "tags", FilterOp::NotExists));
        assert!(!matches(&absent, "refs.id", FilterOp::NotExists));

        for empty in [json!([]), Value::Null] {
            let d = data(&[("tags", empty.clone()), ("refs", empty)]);

            assert!(matches(&d, "tags", FilterOp::NotIn(secret())));
            assert!(matches(&d, "tags", FilterOp::NotExists));
            assert!(!matches(&d, "tags", FilterOp::Exists));
            assert!(matches(&d, "refs.id", FilterOp::NotExists));
        }
    }

    /// A group present as `null` holds NULL columns, so its sub-fields read as
    /// NULL; a group the payload lacks leaves them unknown.
    #[test]
    fn null_group_reads_sub_fields_as_null_absent_group_as_unknown() {
        let fields = vec![
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("owner", FieldType::Text).build(),
                ])
                .build(),
        ];
        let not_exists = [typed_single("seo__owner", FilterOp::NotExists)];

        let null_group = data(&[("seo", Value::Null)]);
        assert!(matches_constraints_typed(&null_group, &not_exists, &fields));

        let no_group = data(&[("title", json!("t"))]);
        assert!(!matches_constraints_typed(&no_group, &not_exists, &fields));
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

    // ── Null values (SQL three-valued logic) ───────────────────────

    // A JSON `null` is a NULL column, mirroring SQL: every comparison against
    // NULL yields NULL (not true), so the row is excluded from `Equals`/`In`
    // *and* from `NotEquals`/`NotIn`. The DB layer maps a SQL NULL column to
    // `Value::Null` (a present key with a null value), so this must not coerce
    // to `""` — doing so would make `NotEquals`/`NotIn` match (fail-open) while
    // SQL excludes the row.

    #[test]
    fn null_value_equals_does_not_match() {
        // SQL: `field = ''` is NULL for a NULL column → row excluded.
        let d = data(&[("field", Value::Null)]);
        assert!(!matches_constraints(&d, &[eq("field", "")]));
        assert!(!matches_constraints(&d, &[eq("field", "something")]));
    }

    /// Regression for the fail-open NULL leak: `NotEquals` against a NULL field
    /// must NOT match, because SQL `field != 'x'` is NULL (excluded) when the
    /// column is NULL. Previously `value_to_string(null) == ""` made `"" != "x"`
    /// true, leaking the row on the in-memory (populate/event/snapshot) paths.
    #[test]
    fn null_value_not_equals_does_not_match() {
        let d = data(&[("field", Value::Null)]);
        assert!(!matches_constraints(&d, &[neq("field", "admin")]));
    }

    /// `In` / `NotIn` against a NULL field both exclude the row (SQL: `field IN
    /// (...)` and `field NOT IN (...)` are NULL for a NULL column).
    #[test]
    fn null_value_membership_does_not_match() {
        let d = data(&[("field", Value::Null)]);
        let in_clause = FilterClause::Single(Filter {
            field: "field".to_string(),
            op: FilterOp::In(vec!["a".to_string(), "b".to_string()]),
        });
        let not_in_clause = FilterClause::Single(Filter {
            field: "field".to_string(),
            op: FilterOp::NotIn(vec!["a".to_string(), "b".to_string()]),
        });
        assert!(!matches_constraints(&d, from_ref(&in_clause)));
        assert!(!matches_constraints(&d, from_ref(&not_in_clause)));
    }

    /// `Exists` is `IS NOT NULL` in SQL, so a NULL field does NOT exist;
    /// `NotExists` (`IS NULL`) does match it.
    #[test]
    fn null_value_exists_semantics_match_sql() {
        let d = data(&[("field", Value::Null)]);
        let exists = FilterClause::Single(Filter {
            field: "field".to_string(),
            op: FilterOp::Exists,
        });
        let not_exists = FilterClause::Single(Filter {
            field: "field".to_string(),
            op: FilterOp::NotExists,
        });
        assert!(!matches_constraints(&d, from_ref(&exists)));
        assert!(matches_constraints(&d, from_ref(&not_exists)));
    }
}
