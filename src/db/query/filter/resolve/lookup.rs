//! Field-tree lookups shared by normalize and `resolve_filter` paths.

use crate::core::{FieldDefinition, FieldType, find_field};

/// Look up the [`FieldType`] for a DB column name on the parent table.
///
/// Handles:
/// - Plain top-level fields (`"status"` → `FieldType::Text`)
/// - Transparent layout wrappers (Row/Collapsible/Tabs)
/// - Group sub-fields using the `{group}__{sub}` double-underscore naming
///   (including nested groups: `a__b__c`)
/// - Optional locale suffix (`field__{locale}`, `group__sub__{locale}`) —
///   the locale segment is the last path component and does not affect the
///   leaf field type.
///
/// Returns `None` when the column cannot be mapped to a known field —
/// callers fall back to `DbValue::Text` binding.
pub(super) fn lookup_column_field_type(col: &str, fields: &[FieldDefinition]) -> Option<FieldType> {
    // Fast path: a top-level scalar/layout leaf named exactly `col`.
    if let Some(f) = find_field(col, fields)
        && !matches!(
            f.field_type,
            FieldType::Group | FieldType::Array | FieldType::Blocks | FieldType::Relationship
        )
    {
        return Some(f.field_type.clone());
    }

    // Group column: split on `__` and walk the tree. If the final segment
    // fails to resolve, drop it and retry — the trailing segment may be a
    // locale suffix (e.g. `title__en`, `meta__description__de`).
    let parts: Vec<&str> = col.split("__").collect();
    if parts.len() < 2 {
        return None;
    }

    if let Some(ft) = walk_group_path(&parts, fields) {
        return Some(ft);
    }

    // Retry without trailing segment to handle locale-suffixed columns.
    if parts.len() >= 2 {
        let without_tail = &parts[..parts.len() - 1];
        return walk_group_path(without_tail, fields);
    }

    None
}

/// Walk a `__`-separated path through Group fields (and transparent layout
/// wrappers) to find the leaf field type.
fn walk_group_path(parts: &[&str], fields: &[FieldDefinition]) -> Option<FieldType> {
    if parts.is_empty() {
        return None;
    }

    let mut current = fields;
    let mut leaf_type: Option<FieldType> = None;

    for (i, seg) in parts.iter().enumerate() {
        let is_last = i == parts.len() - 1;
        let found = find_field(seg, current)?;

        if is_last {
            leaf_type = Some(found.field_type.clone());
            break;
        }

        match found.field_type {
            FieldType::Group => {
                current = &found.fields;
            }
            _ => return None,
        }
    }

    leaf_type
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::field::{FieldDefinition, FieldTab, RelationshipConfig};

    fn scalar(name: &str, ft: FieldType) -> FieldDefinition {
        FieldDefinition::builder(name, ft).build()
    }

    fn group(name: &str, subs: Vec<FieldDefinition>) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Group)
            .fields(subs)
            .build()
    }

    #[test]
    fn top_level_scalars_resolve_to_their_type() {
        let fields = vec![
            scalar("status", FieldType::Text),
            scalar("count", FieldType::Number),
        ];
        assert_eq!(
            lookup_column_field_type("status", &fields),
            Some(FieldType::Text)
        );
        assert_eq!(
            lookup_column_field_type("count", &fields),
            Some(FieldType::Number)
        );
    }

    #[test]
    fn unknown_column_resolves_to_none() {
        let fields = vec![scalar("status", FieldType::Text)];
        assert_eq!(lookup_column_field_type("nope", &fields), None);
    }

    #[test]
    fn non_scalar_columns_resolve_to_none_for_text_fallback() {
        // Relationship/Array/Blocks/bare-Group are not plain scalar columns;
        // callers fall back to Text binding, so these must return None.
        let fields = vec![
            FieldDefinition::builder("author", FieldType::Relationship)
                .relationship(RelationshipConfig::new("users", false))
                .build(),
            FieldDefinition::builder("rows", FieldType::Array)
                .fields(vec![scalar("x", FieldType::Text)])
                .build(),
            FieldDefinition::builder("content", FieldType::Blocks).build(),
            group("meta", vec![scalar("title", FieldType::Text)]),
        ];
        assert_eq!(lookup_column_field_type("author", &fields), None);
        assert_eq!(lookup_column_field_type("rows", &fields), None);
        assert_eq!(lookup_column_field_type("content", &fields), None);
        assert_eq!(lookup_column_field_type("meta", &fields), None);
    }

    #[test]
    fn group_and_nested_group_columns_resolve_the_leaf() {
        let fields = vec![group(
            "meta",
            vec![
                scalar("title", FieldType::Text),
                group("inner", vec![scalar("rank", FieldType::Number)]),
            ],
        )];
        assert_eq!(
            lookup_column_field_type("meta__title", &fields),
            Some(FieldType::Text)
        );
        assert_eq!(
            lookup_column_field_type("meta__inner__rank", &fields),
            Some(FieldType::Number)
        );
    }

    #[test]
    fn trailing_locale_suffix_is_stripped() {
        let fields = vec![
            scalar("title", FieldType::Text),
            group("meta", vec![scalar("desc", FieldType::Textarea)]),
        ];
        // Top-level localized column: `title__en`.
        assert_eq!(
            lookup_column_field_type("title__en", &fields),
            Some(FieldType::Text)
        );
        // Group sub-field localized column: `meta__desc__de`.
        assert_eq!(
            lookup_column_field_type("meta__desc__de", &fields),
            Some(FieldType::Textarea)
        );
    }

    #[test]
    fn layout_wrappers_are_transparent_in_lookup() {
        let fields = vec![
            FieldDefinition::builder("row", FieldType::Row)
                .fields(vec![scalar("first", FieldType::Text)])
                .build(),
            FieldDefinition::builder("tabs", FieldType::Tabs)
                .tabs(vec![FieldTab::new(
                    "SEO",
                    vec![scalar("meta_title", FieldType::Text)],
                )])
                .build(),
        ];
        // Children of Row/Tabs resolve at the top level (no prefix).
        assert_eq!(
            lookup_column_field_type("first", &fields),
            Some(FieldType::Text)
        );
        assert_eq!(
            lookup_column_field_type("meta_title", &fields),
            Some(FieldType::Text)
        );
        assert!(find_field("first", &fields).is_some());
        assert!(find_field("missing", &fields).is_none());
    }
}
