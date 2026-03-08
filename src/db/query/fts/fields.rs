//! Field/column resolution helpers for FTS5 indexing.

use crate::config::LocaleConfig;
use crate::core::CollectionDefinition;
use crate::core::field::FieldType;

/// Determine which logical fields should be indexed in the FTS5 table.
///
/// Uses `list_searchable_fields` if configured, otherwise falls back to all
/// text-like fields (Text, Textarea, Richtext, Email, Code) at the parent level
/// (no group sub-fields, no array/block sub-fields).
pub fn get_fts_fields(def: &CollectionDefinition) -> Vec<String> {
    if !def.admin.list_searchable_fields.is_empty() {
        return def.admin.list_searchable_fields.clone();
    }

    def.fields
        .iter()
        .filter(|f| {
            matches!(
                f.field_type,
                FieldType::Text
                    | FieldType::Textarea
                    | FieldType::Richtext
                    | FieldType::Email
                    | FieldType::Code
            )
        })
        .map(|f| f.name.clone())
        .collect()
}

/// Expand logical field names to actual database column names.
///
/// For non-localized fields, the column name is the field name.
/// For localized fields, each field expands to `field__locale` for each locale.
pub fn get_fts_columns(def: &CollectionDefinition, locale_config: &LocaleConfig) -> Vec<String> {
    let logical_fields = get_fts_fields(def);
    if logical_fields.is_empty() {
        return Vec::new();
    }

    if !locale_config.is_enabled() {
        return logical_fields;
    }

    let mut columns = Vec::new();
    for field_name in &logical_fields {
        let is_localized = def.fields.iter().any(|f| f.name == *field_name && f.localized);

        if is_localized {
            for locale in &locale_config.locales {
                columns.push(format!("{}__{}", field_name, locale));
            }
        } else {
            columns.push(field_name.clone());
        }
    }
    columns
}

/// Build a set of column names that are JSON-format richtext fields.
/// Checks both bare field names and locale-expanded variants (`field__locale`).
pub(super) fn json_richtext_columns(
    def: &CollectionDefinition,
) -> std::collections::HashSet<String> {
    let mut set = std::collections::HashSet::new();
    for f in &def.fields {
        if f.field_type == FieldType::Richtext
            && f.admin.richtext_format.as_deref() == Some("json")
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
    registry: Option<&'a crate::core::Registry>,
) -> std::collections::HashMap<&'a str, Vec<&'a str>> {
    let mut map = std::collections::HashMap::new();
    let (def, registry) = match (def, registry) {
        (Some(d), Some(r)) => (d, r),
        _ => return map,
    };
    for field in &def.fields {
        if field.field_type == FieldType::Richtext
            && field.admin.richtext_format.as_deref() == Some("json")
        {
            for node_name in &field.admin.nodes {
                if let Some(node_def) = registry.get_richtext_node(node_name) {
                    if !node_def.searchable_attrs.is_empty() {
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
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::collection::*;
    use crate::core::field::*;

    fn text_field(name: &str) -> FieldDefinition {
        FieldDefinition {
            name: name.to_string(),
            field_type: FieldType::Text,
            ..Default::default()
        }
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
        FieldDefinition {
            name: name.to_string(),
            field_type: FieldType::Text,
            localized: true,
            ..Default::default()
        }
    }

    #[test]
    fn get_fts_fields_uses_searchable_fields() {
        let mut def = simple_def(vec![
            text_field("title"),
            text_field("body"),
            FieldDefinition { name: "count".to_string(), field_type: FieldType::Number, ..Default::default() },
        ]);
        def.admin.list_searchable_fields = vec!["title".into(), "body".into()];
        assert_eq!(get_fts_fields(&def), vec!["title", "body"]);
    }

    #[test]
    fn get_fts_fields_falls_back_to_text_types() {
        let def = simple_def(vec![
            text_field("title"),
            FieldDefinition {
                name: "body".to_string(),
                field_type: FieldType::Textarea,
                ..Default::default()
            },
            FieldDefinition {
                name: "count".to_string(),
                field_type: FieldType::Number,
                ..Default::default()
            },
            FieldDefinition {
                name: "email".to_string(),
                field_type: FieldType::Email,
                ..Default::default()
            },
            FieldDefinition {
                name: "content".to_string(),
                field_type: FieldType::Richtext,
                ..Default::default()
            },
            FieldDefinition {
                name: "snippet".to_string(),
                field_type: FieldType::Code,
                ..Default::default()
            },
        ]);
        let fields = get_fts_fields(&def);
        assert_eq!(fields, vec!["title", "body", "email", "content", "snippet"]);
    }

    #[test]
    fn get_fts_fields_empty_for_no_text() {
        let def = simple_def(vec![FieldDefinition {
            name: "count".to_string(),
            field_type: FieldType::Number,
            ..Default::default()
        }]);
        assert!(get_fts_fields(&def).is_empty());
    }

    #[test]
    fn get_fts_fields_excludes_non_parent() {
        let def = simple_def(vec![
            text_field("title"),
            FieldDefinition {
                name: "items".to_string(),
                field_type: FieldType::Array,
                fields: vec![text_field("label")],
                ..Default::default()
            },
            FieldDefinition {
                name: "meta".to_string(),
                field_type: FieldType::Group,
                fields: vec![text_field("description")],
                ..Default::default()
            },
        ]);
        // Only "title" at parent level — Array and Group are not text-like
        assert_eq!(get_fts_fields(&def), vec!["title"]);
    }

    #[test]
    fn get_fts_columns_no_locale_returns_field_names() {
        let def = simple_def(vec![text_field("title"), text_field("body")]);
        let cols = get_fts_columns(&def, &LocaleConfig::default());
        assert_eq!(cols, vec!["title", "body"]);
    }

    #[test]
    fn get_fts_columns_with_locale_expands_localized_fields() {
        let def = simple_def(vec![
            localized_text_field("title"),
            localized_text_field("body"),
        ]);
        let cols = get_fts_columns(&def, &locale_config_en_de());
        assert_eq!(cols, vec!["title__en", "title__de", "body__en", "body__de"]);
    }

    #[test]
    fn get_fts_columns_mixed_localized_and_non_localized() {
        let def = simple_def(vec![localized_text_field("title"), text_field("slug")]);
        let cols = get_fts_columns(&def, &locale_config_en_de());
        assert_eq!(cols, vec!["title__en", "title__de", "slug"]);
    }

    #[test]
    fn get_fts_columns_locale_enabled_but_no_localized_fields() {
        let def = simple_def(vec![text_field("title"), text_field("body")]);
        let cols = get_fts_columns(&def, &locale_config_en_de());
        // None of the fields are localized, so no expansion
        assert_eq!(cols, vec!["title", "body"]);
    }

    #[test]
    fn get_fts_columns_empty_when_no_text_fields() {
        let def = simple_def(vec![FieldDefinition {
            name: "count".to_string(),
            field_type: FieldType::Number,
            ..Default::default()
        }]);
        let cols = get_fts_columns(&def, &locale_config_en_de());
        assert!(cols.is_empty());
    }
}
