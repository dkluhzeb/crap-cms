//! Field/column resolution helpers for FTS5 indexing.

use std::collections::HashMap;

use anyhow::Result;

use crate::{
    config::LocaleConfig,
    core::{CollectionDefinition, FieldDefinition, FieldType, Registry},
    db::query::sanitize_locale,
};

/// Field types that live in separate tables and have no column on the parent table.
const CONTAINER_FIELD_TYPES: &[FieldType] = &[
    FieldType::Array,
    FieldType::Blocks,
    FieldType::Group,
    FieldType::Row,
    FieldType::Collapsible,
    FieldType::Tabs,
];

/// Determine which logical fields should be indexed in the FTS5 table.
///
/// Uses `list_searchable_fields` if configured, otherwise falls back to all
/// text-like fields (Text, Textarea, Richtext, Email, Code) at the parent level
/// (no group sub-fields, no array/block sub-fields).
///
/// Container types (array, blocks, group, row, collapsible, tabs) are always
/// excluded because they don't have columns on the parent table.
pub fn get_fts_fields(def: &CollectionDefinition) -> Vec<String> {
    if !def.admin.list_searchable_fields.is_empty() {
        // Only keep fields that actually exist as columns on the parent table.
        // Exclude: container types (stored in separate tables) and names that
        // don't match any field definition at all.
        return def
            .admin
            .list_searchable_fields
            .iter()
            .filter(|name| is_fts_eligible_field(name, &def.fields))
            .cloned()
            .collect();
    }

    collect_fts_defaults(&def.fields)
}

/// Check if a field name refers to an FTS-eligible column, recursing into
/// layout wrappers (Row, Collapsible, Tabs) that promote children.
fn is_fts_eligible_field(name: &str, fields: &[FieldDefinition]) -> bool {
    fields.iter().any(|f| {
        if f.name == name && !CONTAINER_FIELD_TYPES.contains(&f.field_type) {
            return true;
        }

        if matches!(
            f.field_type,
            FieldType::Row | FieldType::Collapsible | FieldType::Tabs
        ) {
            return is_fts_eligible_field(name, &f.fields);
        }

        false
    })
}

/// Collect default FTS fields (text-like) from top level and layout wrappers.
fn collect_fts_defaults(fields: &[FieldDefinition]) -> Vec<String> {
    let mut result = Vec::new();

    for f in fields {
        if matches!(
            f.field_type,
            FieldType::Text
                | FieldType::Textarea
                | FieldType::Richtext
                | FieldType::Email
                | FieldType::Code
        ) {
            result.push(f.name.clone());
        }

        if matches!(
            f.field_type,
            FieldType::Row | FieldType::Collapsible | FieldType::Tabs
        ) {
            result.extend(collect_fts_defaults(&f.fields));
        }
    }

    result
}

/// Expand logical field names to actual database column names.
///
/// For non-localized fields, the column name is the field name.
/// For localized fields, each field expands to `field__locale` for each locale.
pub fn get_fts_columns(
    def: &CollectionDefinition,
    locale_config: &LocaleConfig,
) -> Result<Vec<String>> {
    let logical_fields = get_fts_fields(def);

    if logical_fields.is_empty() {
        return Ok(Vec::new());
    }

    if !locale_config.is_enabled() {
        return Ok(logical_fields);
    }

    let mut columns = Vec::new();
    for field_name in &logical_fields {
        let is_localized = def
            .fields
            .iter()
            .any(|f| f.name == *field_name && f.localized);

        if is_localized {
            for locale in &locale_config.locales {
                columns.push(format!("{}__{}", field_name, sanitize_locale(locale)?));
            }
        } else {
            columns.push(field_name.clone());
        }
    }

    Ok(columns)
}

/// Build a set of column names that are JSON-format richtext fields.
/// Checks both bare field names and locale-expanded variants (`field__locale`).
pub(super) fn json_richtext_columns(
    def: &CollectionDefinition,
) -> std::collections::HashSet<String> {
    let mut set = std::collections::HashSet::new();
    for f in &def.fields {
        if f.field_type == FieldType::Richtext && f.admin.richtext_format.as_deref() == Some("json")
        {
            set.insert(f.name.clone());
        }
    }
    set
}

/// Build a map of node type name → searchable attr names from collection definition
/// and registry. Used for FTS extraction of custom richtext node content.
pub(super) fn build_node_searchable_map<'a>(
    def: Option<&'a CollectionDefinition>,
    registry: Option<&'a Registry>,
) -> HashMap<&'a str, Vec<&'a str>> {
    let mut map = HashMap::new();
    let (def, registry) = match (def, registry) {
        (Some(d), Some(r)) => (d, r),
        _ => return map,
    };
    for field in &def.fields {
        if field.field_type == FieldType::Richtext
            && field.admin.richtext_format.as_deref() == Some("json")
        {
            for node_name in &field.admin.nodes {
                if let Some(node_def) = registry.get_richtext_node(node_name)
                    && !node_def.searchable_attrs.is_empty()
                {
                    map.insert(
                        node_def.name.as_str(),
                        node_def
                            .searchable_attrs
                            .iter()
                            .map(|s| s.as_str())
                            .collect(),
                    );
                }
            }
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::collection::*;
    use crate::core::field::*;

    fn text_field(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Text).build()
    }

    fn simple_def(fields: Vec<FieldDefinition>) -> CollectionDefinition {
        let mut def = CollectionDefinition::new("posts");
        def.fields = fields;
        def
    }

    fn locale_config_en_de() -> LocaleConfig {
        LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: false,
        }
    }

    fn localized_text_field(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Text)
            .localized(true)
            .build()
    }

    #[test]
    fn get_fts_fields_uses_searchable_fields() {
        let mut def = simple_def(vec![
            text_field("title"),
            text_field("body"),
            FieldDefinition::builder("count", FieldType::Number).build(),
        ]);
        def.admin.list_searchable_fields = vec!["title".into(), "body".into()];
        assert_eq!(get_fts_fields(&def), vec!["title", "body"]);
    }

    #[test]
    fn get_fts_fields_falls_back_to_text_types() {
        let def = simple_def(vec![
            text_field("title"),
            FieldDefinition::builder("body", FieldType::Textarea).build(),
            FieldDefinition::builder("count", FieldType::Number).build(),
            FieldDefinition::builder("email", FieldType::Email).build(),
            FieldDefinition::builder("content", FieldType::Richtext).build(),
            FieldDefinition::builder("snippet", FieldType::Code).build(),
        ]);
        let fields = get_fts_fields(&def);
        assert_eq!(fields, vec!["title", "body", "email", "content", "snippet"]);
    }

    #[test]
    fn get_fts_fields_empty_for_no_text() {
        let def = simple_def(vec![
            FieldDefinition::builder("count", FieldType::Number).build(),
        ]);
        assert!(get_fts_fields(&def).is_empty());
    }

    #[test]
    fn get_fts_fields_excludes_non_parent() {
        let def = simple_def(vec![
            text_field("title"),
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![text_field("label")])
                .build(),
            FieldDefinition::builder("meta", FieldType::Group)
                .fields(vec![text_field("description")])
                .build(),
        ]);
        // Only "title" at parent level — Array and Group are not text-like
        assert_eq!(get_fts_fields(&def), vec!["title"]);
    }

    #[test]
    fn get_fts_columns_no_locale_returns_field_names() {
        let def = simple_def(vec![text_field("title"), text_field("body")]);
        let cols = get_fts_columns(&def, &LocaleConfig::default()).unwrap();
        assert_eq!(cols, vec!["title", "body"]);
    }

    #[test]
    fn get_fts_columns_with_locale_expands_localized_fields() {
        let def = simple_def(vec![
            localized_text_field("title"),
            localized_text_field("body"),
        ]);
        let cols = get_fts_columns(&def, &locale_config_en_de()).unwrap();
        assert_eq!(cols, vec!["title__en", "title__de", "body__en", "body__de"]);
    }

    #[test]
    fn get_fts_columns_mixed_localized_and_non_localized() {
        let def = simple_def(vec![localized_text_field("title"), text_field("slug")]);
        let cols = get_fts_columns(&def, &locale_config_en_de()).unwrap();
        assert_eq!(cols, vec!["title__en", "title__de", "slug"]);
    }

    #[test]
    fn get_fts_columns_locale_enabled_but_no_localized_fields() {
        let def = simple_def(vec![text_field("title"), text_field("body")]);
        let cols = get_fts_columns(&def, &locale_config_en_de()).unwrap();
        // None of the fields are localized, so no expansion
        assert_eq!(cols, vec!["title", "body"]);
    }

    #[test]
    fn get_fts_columns_empty_when_no_text_fields() {
        let def = simple_def(vec![
            FieldDefinition::builder("count", FieldType::Number).build(),
        ]);
        let cols = get_fts_columns(&def, &locale_config_en_de()).unwrap();
        assert!(cols.is_empty());
    }

    #[test]
    fn get_fts_fields_excludes_container_from_searchable() {
        // Even when user explicitly lists an array field in list_searchable_fields,
        // it should be filtered out since array fields have no parent table column.
        let mut def = simple_def(vec![
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![text_field("label")])
                .build(),
            text_field("title"),
            FieldDefinition::builder("meta", FieldType::Group)
                .fields(vec![text_field("description")])
                .build(),
        ]);
        def.admin.list_searchable_fields = vec!["items".into(), "title".into(), "meta".into()];
        // Only "title" should survive — "items" (array) and "meta" (group) are excluded
        assert_eq!(get_fts_fields(&def), vec!["title"]);
    }

    #[test]
    fn get_fts_fields_excludes_nonexistent_from_searchable() {
        // A field name in list_searchable_fields that doesn't match any field definition
        // should be silently filtered out (e.g. scaffolded default "title" when no title exists).
        let mut def = simple_def(vec![
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![text_field("label")])
                .build(),
        ]);
        def.admin.list_searchable_fields = vec!["title".into(), "nonexistent".into()];
        // Neither exists as a scalar field — result should be empty
        assert!(get_fts_fields(&def).is_empty());
    }

    // ── Regression: fields inside layout wrappers ────────────────────

    #[test]
    fn searchable_field_inside_row() {
        let mut def = simple_def(vec![FieldDefinition {
            name: "date_row".to_string(),
            field_type: FieldType::Row,
            fields: vec![text_field("title"), text_field("subtitle")],
            ..Default::default()
        }]);
        def.admin.list_searchable_fields = vec!["title".into()];

        assert_eq!(get_fts_fields(&def), vec!["title"]);
    }

    #[test]
    fn searchable_field_inside_collapsible() {
        let mut def = simple_def(vec![FieldDefinition {
            name: "meta".to_string(),
            field_type: FieldType::Collapsible,
            fields: vec![text_field("description")],
            ..Default::default()
        }]);
        def.admin.list_searchable_fields = vec!["description".into()];

        assert_eq!(get_fts_fields(&def), vec!["description"]);
    }

    #[test]
    fn default_fts_includes_fields_inside_wrappers() {
        let def = simple_def(vec![
            text_field("top_level"),
            FieldDefinition {
                name: "row".to_string(),
                field_type: FieldType::Row,
                fields: vec![text_field("nested_in_row")],
                ..Default::default()
            },
        ]);

        let fields = get_fts_fields(&def);
        assert!(fields.contains(&"top_level".to_string()));
        assert!(fields.contains(&"nested_in_row".to_string()));
    }
}
