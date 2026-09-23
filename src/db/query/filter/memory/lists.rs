//! List-valued constraints for the in-memory filter evaluator: a scalar
//! has-many list and a has-many relationship's ids, read element by element
//! exactly as the SQL builder reads them (see `filter::elements`).

use serde_json::Value;

use super::matches_value;
use crate::{
    core::FieldType,
    db::{
        FilterOp,
        query::{
            filter::elements::{Quantifier, quantify},
            helpers::{ListPlace, parse_has_many_scalar},
            poly_ref,
        },
    },
};

/// The elements of a scalar has-many list stored at `place` as the read
/// decoding has them — a missing or null list holds none.
pub(super) fn list_elements(
    value: Option<&Value>,
    field_type: &FieldType,
    place: ListPlace,
) -> Vec<Value> {
    match value.map(|v| parse_has_many_scalar(field_type, v, place)) {
        Some(Value::Array(elements)) => elements,
        _ => Vec::new(),
    }
}

/// The ids a has-many relationship/upload value holds, as its junction rows
/// hold them: an id, the id half of a polymorphic `collection/id` reference,
/// or the `id` of a populated document.
pub(super) fn reference_ids(value: Option<&Value>, polymorphic: bool) -> Vec<Value> {
    let Some(Value::Array(items)) = value else {
        return Vec::new();
    };

    items
        .iter()
        .filter_map(|item| match item {
            Value::String(id) if polymorphic => poly_ref::parse(id).map(|(_, id)| id),
            Value::String(id) => Some(id.clone()),
            Value::Object(doc) => doc.get("id").and_then(Value::as_str).map(str::to_string),
            _ => None,
        })
        .map(Value::String)
        .collect()
}

/// Evaluate a filter on a list, element by element — the reading the SQL
/// builder applies: a negative operator (`not_equals`, `not_in`,
/// `not_exists`) matches when no element matches its positive counterpart,
/// every other operator when some element matches.
pub(super) fn matches_list(
    elements: &[Value],
    op: &FilterOp,
    field_type: Option<&FieldType>,
) -> bool {
    let (quantifier, element_op) = quantify(op);
    let some = elements
        .iter()
        .any(|element| matches_value(element, &element_op, field_type));

    match quantifier {
        Quantifier::Any => some,
        Quantifier::NoElement => !some,
    }
}

#[cfg(test)]
mod tests {
    use std::slice::from_ref;

    use serde_json::{Value, json};

    use crate::{
        core::{DocumentFields, FieldDefinition, FieldType, RelationshipConfig},
        db::{Filter, FilterClause, FilterOp, query::filter::memory::matches_constraints_typed},
    };

    fn data(pairs: &[(&str, Value)]) -> DocumentFields {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    fn typed_single(field: &str, op: FilterOp) -> FilterClause {
        FilterClause::Single(Filter {
            field: field.to_string(),
            op,
        })
    }

    /// Regression: the matcher knew no `rel.id` path, so every has-many
    /// relationship constraint read as a missing field — `not_exists` let
    /// through a document that holds references. The ids are read from the
    /// relationship's value in each shape a document carries them: plain ids,
    /// polymorphic `collection/id` references, populated documents.
    #[test]
    fn has_many_reference_constraints_read_the_ids() {
        let tags = FieldDefinition::builder("tags", FieldType::Relationship)
            .relationship(RelationshipConfig::new("tags", true))
            .build();
        let mut owners_config = RelationshipConfig::new("users", true);
        owners_config.polymorphic = vec!["users".into(), "teams".into()];
        let owners = FieldDefinition::builder("owners", FieldType::Relationship)
            .relationship(owners_config)
            .build();
        let fields = vec![tags, owners];

        let matches = |d: &DocumentFields, c: FilterClause| {
            matches_constraints_typed(d, from_ref(&c), &fields)
        };

        let plain = data(&[
            ("tags", json!(["t1", "t2"])),
            ("owners", json!(["teams/x"])),
        ]);
        assert!(matches(
            &plain,
            typed_single("tags.id", FilterOp::Equals("t2".into()))
        ));
        assert!(!matches(
            &plain,
            typed_single("tags.id", FilterOp::NotEquals("t1".into()))
        ));
        assert!(!matches(
            &plain,
            typed_single("tags.id", FilterOp::NotExists)
        ));
        assert!(matches(
            &plain,
            typed_single("owners.id", FilterOp::Equals("x".into()))
        ));

        let populated = data(&[("tags", json!([{ "id": "t1", "name": "One" }]))]);
        assert!(matches(
            &populated,
            typed_single("tags.id", FilterOp::In(vec!["t1".into()]))
        ));

        let none = data(&[("tags", json!([]))]);
        assert!(matches(&none, typed_single("tags.id", FilterOp::NotExists)));
        assert!(matches(
            &none,
            typed_single("tags.id", FilterOp::NotEquals("t1".into()))
        ));
        assert!(!matches(&none, typed_single("tags.id", FilterOp::Exists)));
    }

    /// A scalar has-many list is matched element by element, whether the
    /// document carries it decoded or as its stored JSON text.
    #[test]
    fn scalar_has_many_constraints_match_element_by_element() {
        let fields = vec![
            FieldDefinition::builder("labels", FieldType::Select)
                .has_many(true)
                .build(),
        ];
        let single = |op: FilterOp| typed_single("labels", op);

        for labels in [json!(["red", "blue"]), json!(r#"["red","blue"]"#)] {
            let d = data(&[("labels", labels)]);

            assert!(matches_constraints_typed(
                &d,
                from_ref(&single(FilterOp::Equals("blue".into()))),
                &fields
            ));
            assert!(!matches_constraints_typed(
                &d,
                from_ref(&single(FilterOp::NotEquals("red".into()))),
                &fields
            ));
            assert!(matches_constraints_typed(
                &d,
                from_ref(&single(FilterOp::NotIn(vec!["green".into()]))),
                &fields
            ));
        }
    }
}
