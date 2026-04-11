//! Sort validation and column eligibility checks.

use crate::core::{collection::CollectionDefinition, field::FieldType};

/// Validate a sort field name against the collection definition.
/// Strips leading `-` (descending) before validation.
/// Returns the validated sort string (with `-` prefix if present), or None.
pub(crate) fn validate_sort(sort: &str, def: &CollectionDefinition) -> Option<String> {
    let field_name = sort.strip_prefix('-').unwrap_or(sort);
    let system_cols = ["id", "created_at", "updated_at", "_status"];
    let valid = system_cols.contains(&field_name)
        || def
            .fields
            .iter()
            .any(|f| f.name == field_name && is_column_eligible(&f.field_type));
    if valid { Some(sort.to_string()) } else { None }
}

/// Check if a field type is eligible for display as a list column.
pub(crate) fn is_column_eligible(field_type: &FieldType) -> bool {
    matches!(
        field_type,
        FieldType::Text
            | FieldType::Email
            | FieldType::Number
            | FieldType::Select
            | FieldType::Checkbox
            | FieldType::Date
            | FieldType::Relationship
            | FieldType::Textarea
            | FieldType::Radio
            | FieldType::Upload
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{collection::CollectionDefinition, field::FieldDefinition};

    fn test_def() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("posts");
        def.timestamps = true;
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("status", FieldType::Select).build(),
            FieldDefinition::builder("body", FieldType::Richtext).build(),
            FieldDefinition::builder("count", FieldType::Number).build(),
        ];
        def
    }

    #[test]
    fn validate_sort_valid_field() {
        let def = test_def();
        assert_eq!(validate_sort("title", &def), Some("title".to_string()));
    }

    #[test]
    fn validate_sort_descending() {
        let def = test_def();
        assert_eq!(validate_sort("-title", &def), Some("-title".to_string()));
    }

    #[test]
    fn validate_sort_system_col() {
        let def = test_def();
        assert_eq!(
            validate_sort("-created_at", &def),
            Some("-created_at".to_string())
        );
    }

    #[test]
    fn validate_sort_invalid() {
        let def = test_def();
        assert_eq!(validate_sort("nonexistent", &def), None);
    }

    #[test]
    fn validate_sort_ineligible_field() {
        let def = test_def();
        assert_eq!(validate_sort("body", &def), None);
    }

    #[test]
    fn column_eligible_text() {
        assert!(is_column_eligible(&FieldType::Text));
        assert!(is_column_eligible(&FieldType::Email));
        assert!(is_column_eligible(&FieldType::Number));
        assert!(is_column_eligible(&FieldType::Select));
        assert!(is_column_eligible(&FieldType::Checkbox));
        assert!(is_column_eligible(&FieldType::Date));
    }

    #[test]
    fn column_ineligible_richtext() {
        assert!(!is_column_eligible(&FieldType::Richtext));
        assert!(!is_column_eligible(&FieldType::Array));
        assert!(!is_column_eligible(&FieldType::Group));
        assert!(!is_column_eligible(&FieldType::Blocks));
        assert!(!is_column_eligible(&FieldType::Json));
        assert!(!is_column_eligible(&FieldType::Code));
        assert!(!is_column_eligible(&FieldType::Join));
    }
}
