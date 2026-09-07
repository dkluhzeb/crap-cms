//! Shape of a relationship / upload value on write: an id string (has-one) or
//! a list of id strings (has-many), `"collection/id"` for polymorphic targets.
//!
//! Anything else — a number, a boolean, a populated document object round-
//! tripped from a `depth > 0` read, a list with non-string items — would
//! otherwise be stored as text: never existence-checked, never ref-counted,
//! and unresolvable on populate. That is a dangling reference the write path
//! promises to fail loudly on, so the shape is rejected here.

use serde_json::Value;

use crate::core::{FieldDefinition, validate::FieldError};

/// Reject a relationship/upload value that is not an id or a list of ids.
///
/// A has-many value may also arrive as a string (the admin form's JSON /
/// comma-separated encodings, decoded by the writer); those are not
/// inspected here. `null` and absent values are left to `required`.
pub(crate) fn check_relationship_shape(
    field: &FieldDefinition,
    data_key: &str,
    value: Option<&Value>,
    errors: &mut Vec<FieldError>,
) {
    if !field.field_type.is_reference() {
        return;
    }

    let Some(rc) = field.relationship.as_ref() else {
        return;
    };

    let Some(value) = value else { return };
    if value.is_null() {
        return;
    }

    let polymorphic = rc.is_polymorphic();
    let problem = if rc.has_many {
        match value {
            Value::Array(items) => items
                .iter()
                .find_map(|item| element_problem(item, polymorphic)),
            Value::String(_) => None,
            _ => Some("must be a list of ids"),
        }
    } else {
        element_problem(value, polymorphic)
    };

    let Some(problem) = problem else { return };

    errors.push(
        FieldError::with_key(
            data_key.to_owned(),
            format!("{} {problem}", field.name),
            "validation.relationship_shape",
        )
        .with_param("field", field.name.clone()),
    );
}

/// Why a single reference value is not an id, if it isn't.
fn element_problem(value: &Value, polymorphic: bool) -> Option<&'static str> {
    match value {
        Value::String(s) if s.is_empty() => None,
        Value::String(s) if polymorphic && !s.contains('/') => {
            Some("must reference its target as 'collection/id' (polymorphic relationship)")
        }
        Value::String(_) => None,
        Value::Object(_) => {
            Some("must be an id, not a populated document — send the referenced document's id")
        }
        _ => Some("must be an id string"),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::core::field::{FieldType, RelationshipConfig};

    fn has_one() -> FieldDefinition {
        FieldDefinition::builder("author", FieldType::Relationship)
            .relationship(RelationshipConfig::new("authors", false))
            .build()
    }

    fn has_many() -> FieldDefinition {
        FieldDefinition::builder("tags", FieldType::Relationship)
            .relationship(RelationshipConfig::new("tags", true))
            .build()
    }

    fn polymorphic() -> FieldDefinition {
        let mut rc = RelationshipConfig::new("posts", false);
        rc.polymorphic = vec!["posts".into(), "pages".into()];
        FieldDefinition::builder("target", FieldType::Relationship)
            .relationship(rc)
            .build()
    }

    fn errors_for(field: &FieldDefinition, value: &Value) -> Vec<FieldError> {
        let mut errors = Vec::new();
        check_relationship_shape(field, &field.name, Some(value), &mut errors);
        errors
    }

    #[test]
    fn id_strings_and_absent_values_pass() {
        assert!(errors_for(&has_one(), &json!("a1")).is_empty());
        assert!(errors_for(&has_one(), &json!("")).is_empty());
        assert!(errors_for(&has_one(), &Value::Null).is_empty());
        assert!(errors_for(&has_many(), &json!(["t1", "t2"])).is_empty());
        assert!(errors_for(&has_many(), &json!("t1,t2")).is_empty());
        assert!(errors_for(&polymorphic(), &json!("pages/p1")).is_empty());

        let mut errors = Vec::new();
        check_relationship_shape(&has_one(), "author", None, &mut errors);
        assert!(errors.is_empty());
    }

    #[test]
    fn non_id_shapes_are_rejected() {
        for bad in [
            json!(12345),
            json!(true),
            json!({"id": "a1", "name": "Ann"}),
        ] {
            let errors = errors_for(&has_one(), &bad);
            assert_eq!(errors.len(), 1, "expected a rejection for {bad}");
            assert_eq!(errors[0].field, "author");
        }

        let errors = errors_for(&has_many(), &json!([1, 2]));
        assert_eq!(errors.len(), 1, "non-string list items are rejected");
        let errors = errors_for(&has_many(), &json!({"id": "t1"}));
        assert_eq!(errors.len(), 1, "an object is not a list of ids");
    }

    #[test]
    fn a_populated_document_names_the_fix() {
        let errors = errors_for(&has_one(), &json!({"id": "a1"}));
        assert!(errors[0].message.contains("populated document"));
    }

    #[test]
    fn polymorphic_ids_need_the_collection_prefix() {
        let errors = errors_for(&polymorphic(), &json!("p1"));
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("collection/id"));
    }

    #[test]
    fn non_reference_fields_are_ignored() {
        let text = FieldDefinition::builder("title", FieldType::Text).build();
        assert!(errors_for(&text, &json!(42)).is_empty());
    }
}
