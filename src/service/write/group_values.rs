//! The shape a write's group values must have: an object of sub-fields.
//!
//! A group has no column of its own — only its sub-fields do — so a group key
//! holding anything but an object has nothing to be stored as. It used to be
//! dropped without a word: `{ seo = crap.null }` read as "clear the group" to
//! the caller and changed nothing. Refused instead, naming the group, on every
//! write surface (they all admit their input through here).

use serde_json::Value;

use crate::{
    core::{
        DocumentFields, FieldDefinition, FieldError, FieldType, ValidationError, find_field,
        prefixed_name,
    },
    service::ServiceError,
};

/// Translation key of the error for a group set to `null`.
const GROUP_NULL_KEY: &str = "validation.group_null";

/// Translation key of the error for a group set to a value that is not an
/// object.
const NOT_AN_OBJECT_KEY: &str = "validation.invalid_row_type";

/// The error for a group key `data_key` (the group's column prefix, e.g.
/// `seo` or `seo__social`) holding `value`, when it is not an object.
fn shape_error(field: &FieldDefinition, data_key: &str, value: &Value) -> Option<FieldError> {
    if value.is_object() {
        return None;
    }

    let error = if value.is_null() {
        FieldError::with_key(
            data_key,
            format!(
                "{} cannot be null — set its sub-fields to null to clear them",
                field.name
            ),
            GROUP_NULL_KEY,
        )
    } else {
        FieldError::with_key(
            data_key,
            format!("{} must be an object", field.name),
            NOT_AN_OBJECT_KEY,
        )
    };

    Some(error.with_param("field", field.name.clone()))
}

/// Check the group values in `values` (a document's top level, or a group's
/// object) against `fields`, descending into nested groups. `prefix` is the
/// column prefix of the enclosing group (empty at the top level).
fn collect<'a>(
    values: impl IntoIterator<Item = (&'a String, &'a Value)>,
    fields: &[FieldDefinition],
    prefix: &str,
    errors: &mut Vec<FieldError>,
) {
    for (key, value) in values {
        let Some(field) = find_field(key, fields).filter(|f| f.field_type == FieldType::Group)
        else {
            continue;
        };

        let data_key = prefixed_name(prefix, key);

        if let Some(error) = shape_error(field, &data_key, value) {
            errors.push(error);
            continue;
        }

        if let Some(object) = value.as_object() {
            collect(object, &field.fields, &data_key, errors);
        }
    }
}

/// Refuse a write whose `data` (canonical, groups nested) sets a group — at
/// the top level or nested in another group — to `null` or to anything else
/// that is not an object of its sub-fields.
///
/// Groups inside array and blocks rows are checked by row validation, where a
/// `null` group is stored as the row's value.
///
/// # Errors
///
/// Returns a validation error naming each offending group.
pub(crate) fn reject_non_object_groups(
    data: &DocumentFields,
    fields: &[FieldDefinition],
) -> Result<(), ServiceError> {
    let mut errors = Vec::new();

    collect(data, fields, "", &mut errors);

    if errors.is_empty() {
        return Ok(());
    }

    Err(ServiceError::Validation(ValidationError::new(errors)))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn text(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Text).build()
    }

    fn schema() -> Vec<FieldDefinition> {
        let social = FieldDefinition::builder("social", FieldType::Group)
            .fields(vec![text("og_title")])
            .build();
        let seo = FieldDefinition::builder("seo", FieldType::Group)
            .fields(vec![text("meta_title"), social])
            .build();
        let row = FieldDefinition::builder("layout", FieldType::Row)
            .fields(vec![
                FieldDefinition::builder("meta", FieldType::Group)
                    .fields(vec![text("note")])
                    .build(),
            ])
            .build();

        vec![text("title"), seo, row]
    }

    fn data(value: &Value) -> DocumentFields {
        value
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    fn error_fields(result: Result<(), ServiceError>) -> Vec<String> {
        let Err(ServiceError::Validation(ve)) = result else {
            panic!("expected a validation error");
        };

        ve.errors.iter().map(|e| e.field.clone()).collect()
    }

    /// Regression: a group set to `null` was dropped without a word, so a
    /// caller clearing it believed it had.
    #[test]
    fn a_null_group_is_refused_naming_the_group() {
        let result = reject_non_object_groups(&data(&json!({ "seo": null })), &schema());

        let Err(ServiceError::Validation(ve)) = &result else {
            panic!("expected a validation error");
        };
        assert_eq!(ve.errors[0].field, "seo");
        assert_eq!(ve.errors[0].key.as_deref(), Some(GROUP_NULL_KEY));
        assert!(ve.errors[0].message.contains("sub-fields to null"));
    }

    #[test]
    fn a_nested_null_group_is_refused_by_its_column_prefix() {
        let result = reject_non_object_groups(
            &data(&json!({ "seo": { "meta_title": "T", "social": null } })),
            &schema(),
        );

        assert_eq!(error_fields(result), vec!["seo__social"]);
    }

    /// A group inside a layout wrapper is addressed by its own name.
    #[test]
    fn a_group_inside_a_row_is_checked() {
        let result = reject_non_object_groups(&data(&json!({ "meta": 5 })), &schema());

        assert_eq!(error_fields(result), vec!["meta"]);
    }

    #[test]
    fn a_non_object_group_value_is_refused() {
        let result = reject_non_object_groups(&data(&json!({ "seo": "x" })), &schema());

        let Err(ServiceError::Validation(ve)) = result else {
            panic!("expected a validation error");
        };
        assert_eq!(ve.errors[0].key.as_deref(), Some(NOT_AN_OBJECT_KEY));
    }

    /// Clearing a group's sub-fields, and every non-group value, is accepted.
    #[test]
    fn sub_fields_set_to_null_and_plain_values_pass() {
        let ok = data(&json!({
            "title": null,
            "seo": { "meta_title": null, "social": { "og_title": null } },
            "meta": {}
        }));

        assert!(reject_non_object_groups(&ok, &schema()).is_ok());
    }
}
