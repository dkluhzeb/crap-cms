//! Polymorphic relationship allowlist validation.
//!
//! A polymorphic relationship field declares a list of allowed target
//! collections via `relationship.polymorphic = ["posts", "articles"]`.
//! Without this check, the write path in
//! `db::query::join::hydrate::save::save_join_data_inner` (and the
//! scalar-column write for non-`has_many`) accepts any submitted
//! `(collection, id)` pair, including collections outside the allowlist.
//! The stored ref then leaks at enrich time as a label from a collection
//! the field author never intended to expose.
//!
//! Polymorphic values arrive in one of two shapes:
//! - `has_many = true`: an array of `"collection/id"` strings
//!   (or objects `{"collection": "...", "id": "..."}`)
//! - `has_many = false`: a single `"collection/id"` string
//!
//! Both shapes are validated here.
//!
//! The allowlist check fires only on polymorphic relationships and is a
//! no-op for plain (non-polymorphic) relationships, where the target
//! collection is fixed by the field config and unforgeable.

use std::collections::HashMap;

use serde_json::Value;

use crate::core::{FieldDefinition, FieldType, validate::FieldError};

/// Reject polymorphic relationship values whose target collection is not
/// in the field's `polymorphic` allowlist.
pub(crate) fn check_polymorphic_allowlist(
    field: &FieldDefinition,
    data_key: &str,
    value: Option<&Value>,
    errors: &mut Vec<FieldError>,
) {
    if !matches!(
        field.field_type,
        FieldType::Relationship | FieldType::Upload
    ) {
        return;
    }

    let Some(rc) = field.relationship.as_ref() else {
        return;
    };

    if !rc.is_polymorphic() {
        return;
    }

    let Some(value) = value else { return };

    if rc.has_many {
        let items = match value {
            Value::Array(arr) => arr.clone(),
            // Comma-separated string falls through to the parser below.
            Value::String(s) if !s.is_empty() => s
                .split(',')
                .map(|p| Value::String(p.trim().to_string()))
                .collect(),
            _ => return,
        };
        for item in items {
            check_one(field, data_key, &item, rc, errors);
        }
    } else {
        check_one(field, data_key, value, rc, errors);
    }
}

fn check_one(
    field: &FieldDefinition,
    data_key: &str,
    value: &Value,
    rc: &crate::core::field::RelationshipConfig,
    errors: &mut Vec<FieldError>,
) {
    let collection = match value {
        Value::String(s) if s.is_empty() => return,
        Value::String(s) => s.split_once('/').map(|(c, _)| c.to_string()),
        Value::Object(obj) => obj
            .get("collection")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        Value::Null => return,
        _ => None,
    };

    let Some(collection) = collection else {
        // Malformed shape — leave to other validators (required, type
        // coercion). The allowlist check only fires once we know which
        // collection the user is trying to reference.
        return;
    };

    let allowed: Vec<&str> = rc.polymorphic.iter().map(|s| s.as_ref()).collect();
    if allowed.contains(&collection.as_str()) {
        return;
    }

    errors.push(FieldError::with_key(
        data_key.to_owned(),
        format!(
            "{} references collection '{}' which is not in the polymorphic allowlist {:?}",
            field.name, collection, allowed
        ),
        "validation.polymorphic_collection_not_allowed",
        HashMap::from([
            ("field".to_string(), field.name.clone()),
            ("collection".to_string(), collection),
            ("allowed".to_string(), allowed.join(",")),
        ]),
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::field::{FieldDefinition, FieldType, RelationshipConfig};
    use serde_json::json;

    fn polymorphic_field(has_many: bool, allowlist: &[&str]) -> FieldDefinition {
        let mut rc = RelationshipConfig::new("", has_many);
        rc.polymorphic = allowlist.iter().map(|s| (*s).into()).collect();
        FieldDefinition::builder("ref", FieldType::Relationship)
            .relationship(rc)
            .has_many(has_many)
            .build()
    }

    #[test]
    fn allowed_collection_passes_scalar() {
        let field = polymorphic_field(false, &["posts", "articles"]);
        let mut errors = Vec::new();
        check_polymorphic_allowlist(&field, "ref", Some(&json!("posts/p1")), &mut errors);
        assert!(errors.is_empty());
    }

    #[test]
    fn disallowed_collection_fails_scalar() {
        let field = polymorphic_field(false, &["posts", "articles"]);
        let mut errors = Vec::new();
        check_polymorphic_allowlist(&field, "ref", Some(&json!("secret/s1")), &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("secret"));
        assert!(errors[0].message.contains("polymorphic allowlist"));
    }

    #[test]
    fn allowed_collection_passes_has_many_array() {
        let field = polymorphic_field(true, &["posts", "articles"]);
        let mut errors = Vec::new();
        let val = json!(["posts/p1", "articles/a1"]);
        check_polymorphic_allowlist(&field, "ref", Some(&val), &mut errors);
        assert!(errors.is_empty(), "got: {:?}", errors);
    }

    #[test]
    fn one_disallowed_in_has_many_fails_loudly() {
        let field = polymorphic_field(true, &["posts"]);
        let mut errors = Vec::new();
        let val = json!(["posts/p1", "articles/a1"]);
        check_polymorphic_allowlist(&field, "ref", Some(&val), &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("articles"));
    }

    #[test]
    fn object_shape_with_collection_field_validated() {
        let field = polymorphic_field(true, &["posts"]);
        let mut errors = Vec::new();
        let val = json!([{"collection": "secret", "id": "s1"}]);
        check_polymorphic_allowlist(&field, "ref", Some(&val), &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("secret"));
    }

    #[test]
    fn empty_value_skipped() {
        let field = polymorphic_field(false, &["posts"]);
        let mut errors = Vec::new();
        check_polymorphic_allowlist(&field, "ref", Some(&json!("")), &mut errors);
        assert!(errors.is_empty());
    }

    #[test]
    fn null_value_skipped() {
        let field = polymorphic_field(false, &["posts"]);
        let mut errors = Vec::new();
        check_polymorphic_allowlist(&field, "ref", Some(&Value::Null), &mut errors);
        assert!(errors.is_empty());
    }

    #[test]
    fn non_polymorphic_relationship_is_no_op() {
        // Plain has-one relationship to "posts" — no polymorphic list.
        let mut rc = RelationshipConfig::new("posts", false);
        rc.polymorphic = vec![];
        let field = FieldDefinition::builder("author", FieldType::Relationship)
            .relationship(rc)
            .build();
        let mut errors = Vec::new();
        // Even with a value that LOOKS polymorphic, no allowlist applies.
        check_polymorphic_allowlist(&field, "author", Some(&json!("anything/x")), &mut errors);
        assert!(errors.is_empty());
    }
}
