//! Sort validation and column eligibility checks.

use crate::core::{CollectionDefinition, FieldDefinition, FieldType};

/// Validate a sort field name against the collection definition.
/// Strips leading `-` (descending) before validation.
/// Returns the validated sort string (with `-` prefix if present), or None.
pub(crate) fn validate_sort(sort: &str, def: &CollectionDefinition) -> Option<String> {
    let field_name = sort.strip_prefix('-').unwrap_or(sort);
    if is_sortable_column(field_name, def) {
        Some(sort.to_string())
    } else {
        None
    }
}

/// Whether `key` names a system column the collection's table actually has —
/// the SINGLE source of truth, shared by the sort gate below, the list-view
/// header/column resolver and the saved column preferences.
///
/// `_status` exists only on a collection that keeps drafts, and the
/// timestamps only on one defined with `timestamps` (the default). Accepting
/// them unconditionally let `?sort=_status` and a saved `_status` column
/// through to a query naming a column that was never created, which answers
/// an error page instead of the 400 an unknown key owes.
#[must_use]
pub(crate) fn is_meta_column(key: &str, def: &CollectionDefinition) -> bool {
    match key {
        "created_at" | "updated_at" => def.timestamps,
        "_status" => def.has_drafts(),
        _ => false,
    }
}

/// Whether `key` may be sorted on — the SINGLE source of truth for
/// sortability, shared by [`validate_sort`] (which rejects a bad
/// `?sort=`) and the list-view column builder (which decides whether to
/// render a clickable sort header). A has-many relationship is
/// column-eligible but has no parent column, so it is NOT sortable —
/// rendering a sort header for it produced a 400 on click when the two
/// predicates disagreed. A scalar has-many list has a column, but its values
/// have no order, so it is not sortable either.
///
/// `id` is sortable but is not a *column* the list view offers, so it sits
/// here rather than in [`is_meta_column`].
#[must_use]
pub(crate) fn is_sortable_column(key: &str, def: &CollectionDefinition) -> bool {
    key == "id"
        || is_meta_column(key, def)
        || def
            .fields
            .iter()
            .any(|f| f.name == key && is_sortable_field(f))
}

/// Whether a top-level field orders the list: one value per document in a
/// column of the collection's own table, of a type the list shows.
fn is_sortable_field(field: &FieldDefinition) -> bool {
    field.has_parent_column()
        && !field.is_has_many_scalar()
        && is_column_eligible(&field.field_type)
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
    use crate::core::{RelationshipConfig, VersionsConfig};

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

    /// Regression: `_status` is a column only on a collection that keeps
    /// drafts. Sorting by it elsewhere reached SQL naming a column the table
    /// never had — a 500 where an unknown sort key owes a 400.
    #[test]
    fn validate_sort_status_needs_drafts() {
        let def = test_def();
        assert_eq!(validate_sort("_status", &def), None, "no drafts, no column");
        assert!(!is_meta_column("_status", &def));

        let mut with_drafts = test_def();
        with_drafts.versions = Some(VersionsConfig::new(true, 10));
        assert_eq!(
            validate_sort("-_status", &with_drafts),
            Some("-_status".to_string())
        );
        assert!(is_meta_column("_status", &with_drafts));
    }

    /// Regression: a collection defined with `timestamps = false` has no
    /// `created_at`/`updated_at` columns, so they are neither list columns
    /// nor sort keys there.
    #[test]
    fn timestamps_are_meta_columns_only_with_timestamps() {
        let mut def = test_def();
        def.timestamps = false;

        for key in ["created_at", "updated_at"] {
            assert!(!is_meta_column(key, &def), "{key}");
            assert_eq!(validate_sort(&format!("-{key}"), &def), None, "{key}");
        }

        assert_eq!(validate_sort("id", &def), Some("id".to_string()));
    }

    /// The timestamp columns exist on a collection with timestamps; `id` is
    /// sortable but is not a list column.
    #[test]
    fn meta_columns_are_the_timestamps() {
        let def = test_def();
        assert!(is_meta_column("created_at", &def));
        assert!(is_meta_column("updated_at", &def));
        assert!(!is_meta_column("id", &def));
        assert!(is_sortable_column("id", &def));
        assert!(!is_meta_column("title", &def));
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

    /// Regression: a has-many relationship has no parent column, so sorting by
    /// it must be rejected at the 400 gate — not accepted here and then 500 at
    /// the DB layer when the ORDER BY column doesn't exist.
    #[test]
    fn validate_sort_rejects_has_many_relationship() {
        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            FieldDefinition {
                name: "tags".to_string(),
                field_type: FieldType::Relationship,
                relationship: Some(RelationshipConfig::new("tags", true)),
                ..Default::default()
            },
            FieldDefinition {
                name: "author".to_string(),
                field_type: FieldType::Relationship,
                relationship: Some(RelationshipConfig::new("users", false)),
                ..Default::default()
            },
        ];

        assert_eq!(validate_sort("tags", &def), None, "has-many not sortable");
        assert_eq!(
            validate_sort("author", &def),
            Some("author".to_string()),
            "has-one relationship remains sortable",
        );
    }

    /// A scalar has-many list has a column but no order: sorting by it would
    /// order the stored JSON text, so it gets no sort header and `?sort=` on it
    /// is refused.
    #[test]
    fn validate_sort_rejects_scalar_has_many() {
        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            FieldDefinition::builder("tags", FieldType::Select)
                .has_many(true)
                .build(),
            FieldDefinition::builder("status", FieldType::Select).build(),
        ];

        assert_eq!(validate_sort("-tags", &def), None);
        assert!(!is_sortable_column("tags", &def));
        assert_eq!(validate_sort("status", &def), Some("status".to_string()));
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
