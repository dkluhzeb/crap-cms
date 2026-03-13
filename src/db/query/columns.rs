//! Column name collection from field trees.

use std::collections::HashSet;

use crate::core::{
    CollectionDefinition,
    field::{FieldDefinition, FieldType},
};

use super::locale::LocaleContext;

/// Get column names for a collection (id + field columns + timestamps).
pub fn get_column_names(def: &CollectionDefinition) -> Vec<String> {
    let mut names = vec!["id".to_string()];
    collect_column_names(&def.fields, &mut names);

    if def.has_drafts() {
        names.push("_status".to_string());
    }
    if def.timestamps {
        names.push("created_at".to_string());
        names.push("updated_at".to_string());
    }
    names
}

/// Recursively collect column names from a field tree.
pub fn collect_column_names(fields: &[FieldDefinition], names: &mut Vec<String>) {
    collect_column_names_inner(fields, names, "");
}

fn collect_column_names_inner(fields: &[FieldDefinition], names: &mut Vec<String>, prefix: &str) {
    for field in fields {
        match field.field_type {
            FieldType::Group => {
                let new_prefix = if prefix.is_empty() {
                    field.name.clone()
                } else {
                    format!("{}__{}", prefix, field.name)
                };
                collect_column_names_inner(&field.fields, names, &new_prefix);
            }
            FieldType::Row | FieldType::Collapsible => {
                collect_column_names_inner(&field.fields, names, prefix);
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    collect_column_names_inner(&tab.fields, names, prefix);
                }
            }
            _ => {
                if field.has_parent_column() {
                    let col = if prefix.is_empty() {
                        field.name.clone()
                    } else {
                        format!("{}__{}", prefix, field.name)
                    };
                    names.push(col);
                }
            }
        }
    }
}

/// Get the set of valid filter column names, accounting for locale.
pub(crate) fn get_valid_filter_columns(
    def: &CollectionDefinition,
    locale_ctx: Option<&LocaleContext>,
) -> HashSet<String> {
    let mut valid = HashSet::new();
    valid.insert("id".to_string());
    collect_valid_filter_names(&def.fields, &mut valid, "");

    if def.has_drafts() {
        valid.insert("_status".to_string());
    }
    if def.timestamps {
        valid.insert("created_at".to_string());
        valid.insert("updated_at".to_string());
    }
    let _ = locale_ctx; // filter validation uses undecorated field names
    valid
}

fn collect_valid_filter_names(
    fields: &[FieldDefinition],
    valid: &mut HashSet<String>,
    prefix: &str,
) {
    for field in fields {
        match field.field_type {
            FieldType::Group => {
                let new_prefix = if prefix.is_empty() {
                    field.name.clone()
                } else {
                    format!("{}__{}", prefix, field.name)
                };
                collect_valid_filter_names(&field.fields, valid, &new_prefix);
            }
            FieldType::Row | FieldType::Collapsible => {
                collect_valid_filter_names(&field.fields, valid, prefix);
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    collect_valid_filter_names(&tab.fields, valid, prefix);
                }
            }
            _ => {
                if field.has_parent_column() {
                    let col = if prefix.is_empty() {
                        field.name.clone()
                    } else {
                        format!("{}__{}", prefix, field.name)
                    };
                    valid.insert(col);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::field::{FieldTab, FieldType};
    use crate::db::query::test_helpers::*;

    #[test]
    fn get_column_names_simple_fields() {
        let def = make_collection_def(
            "posts",
            vec![
                make_field("title", FieldType::Text),
                make_field("count", FieldType::Number),
            ],
            true,
        );
        let names = get_column_names(&def);
        assert_eq!(
            names,
            vec!["id", "title", "count", "created_at", "updated_at"]
        );
    }

    #[test]
    fn get_column_names_with_group() {
        let def = make_collection_def(
            "posts",
            vec![
                make_field("title", FieldType::Text),
                make_group_field(
                    "seo",
                    vec![
                        make_field("title", FieldType::Text),
                        make_field("description", FieldType::Textarea),
                    ],
                ),
            ],
            true,
        );
        let names = get_column_names(&def);
        assert_eq!(
            names,
            vec![
                "id",
                "title",
                "seo__title",
                "seo__description",
                "created_at",
                "updated_at"
            ]
        );
    }

    #[test]
    fn get_column_names_no_timestamps() {
        let def = make_collection_def("posts", vec![make_field("title", FieldType::Text)], false);
        let names = get_column_names(&def);
        assert_eq!(names, vec!["id", "title"]);
    }

    #[test]
    fn get_column_names_with_row() {
        let def = make_collection_def(
            "posts",
            vec![make_row_field(
                "layout",
                vec![
                    make_field("first_name", FieldType::Text),
                    make_field("last_name", FieldType::Text),
                ],
            )],
            true,
        );
        let names = get_column_names(&def);
        assert_eq!(
            names,
            vec!["id", "first_name", "last_name", "created_at", "updated_at"]
        );
    }

    #[test]
    fn get_column_names_with_collapsible() {
        let def = make_collection_def(
            "posts",
            vec![make_collapsible_field(
                "extra",
                vec![make_field("notes", FieldType::Textarea)],
            )],
            true,
        );
        let names = get_column_names(&def);
        assert_eq!(names, vec!["id", "notes", "created_at", "updated_at"]);
    }

    #[test]
    fn get_column_names_with_tabs() {
        let def = make_collection_def(
            "posts",
            vec![make_tabs_field(
                "layout",
                vec![
                    FieldTab::new("Content", vec![make_field("body", FieldType::Textarea)]),
                    FieldTab::new("Meta", vec![make_field("slug", FieldType::Text)]),
                ],
            )],
            true,
        );
        let names = get_column_names(&def);
        assert_eq!(
            names,
            vec!["id", "body", "slug", "created_at", "updated_at"]
        );
    }

    #[test]
    fn get_column_names_tabs_containing_group() {
        let def = make_collection_def(
            "posts",
            vec![make_tabs_field(
                "layout",
                vec![
                    FieldTab::new(
                        "Social",
                        vec![make_group_field(
                            "social",
                            vec![
                                make_field("github", FieldType::Text),
                                make_field("twitter", FieldType::Text),
                            ],
                        )],
                    ),
                    FieldTab::new("Content", vec![make_field("body", FieldType::Textarea)]),
                ],
            )],
            true,
        );
        let names = get_column_names(&def);
        assert_eq!(
            names,
            vec![
                "id",
                "social__github",
                "social__twitter",
                "body",
                "created_at",
                "updated_at"
            ]
        );
    }

    #[test]
    fn get_column_names_collapsible_containing_group() {
        let def = make_collection_def(
            "posts",
            vec![make_collapsible_field(
                "extra",
                vec![
                    make_group_field("seo", vec![make_field("title", FieldType::Text)]),
                    make_field("notes", FieldType::Textarea),
                ],
            )],
            true,
        );
        let names = get_column_names(&def);
        assert_eq!(
            names,
            vec!["id", "seo__title", "notes", "created_at", "updated_at"]
        );
    }

    #[test]
    fn get_column_names_deeply_nested_tabs_collapsible_group() {
        let def = make_collection_def(
            "posts",
            vec![make_tabs_field(
                "layout",
                vec![FieldTab::new(
                    "Advanced",
                    vec![make_collapsible_field(
                        "advanced",
                        vec![
                            make_group_field("og", vec![make_field("image", FieldType::Text)]),
                            make_field("canonical", FieldType::Text),
                        ],
                    )],
                )],
            )],
            false,
        );
        let names = get_column_names(&def);
        assert_eq!(names, vec!["id", "og__image", "canonical"]);
    }

    #[test]
    fn get_column_names_group_containing_row() {
        let fields = vec![make_group_field(
            "meta",
            vec![make_row_field(
                "r",
                vec![
                    make_field("title", FieldType::Text),
                    make_field("slug", FieldType::Text),
                ],
            )],
        )];
        let def = make_collection_def("posts", fields, false);
        let names = get_column_names(&def);
        assert!(
            names.contains(&"meta__title".to_string()),
            "Group→Row: meta__title"
        );
        assert!(
            names.contains(&"meta__slug".to_string()),
            "Group→Row: meta__slug"
        );
    }

    #[test]
    fn get_column_names_group_containing_collapsible() {
        let fields = vec![make_group_field(
            "seo",
            vec![make_collapsible_field(
                "c",
                vec![
                    make_field("robots", FieldType::Text),
                    make_field("canonical", FieldType::Text),
                ],
            )],
        )];
        let def = make_collection_def("posts", fields, false);
        let names = get_column_names(&def);
        assert!(
            names.contains(&"seo__robots".to_string()),
            "Group→Collapsible: seo__robots"
        );
        assert!(
            names.contains(&"seo__canonical".to_string()),
            "Group→Collapsible: seo__canonical"
        );
    }

    #[test]
    fn get_column_names_group_containing_tabs() {
        let fields = vec![make_group_field(
            "settings",
            vec![make_tabs_field(
                "t",
                vec![
                    FieldTab::new("General", vec![make_field("theme", FieldType::Text)]),
                    FieldTab::new("Advanced", vec![make_field("cache_ttl", FieldType::Text)]),
                ],
            )],
        )];
        let def = make_collection_def("posts", fields, false);
        let names = get_column_names(&def);
        assert!(
            names.contains(&"settings__theme".to_string()),
            "Group→Tabs: settings__theme"
        );
        assert!(
            names.contains(&"settings__cache_ttl".to_string()),
            "Group→Tabs: settings__cache_ttl"
        );
    }

    #[test]
    fn get_column_names_group_tabs_group_three_levels() {
        let fields = vec![make_group_field(
            "outer",
            vec![make_tabs_field(
                "t",
                vec![FieldTab::new(
                    "Nested",
                    vec![make_group_field(
                        "inner",
                        vec![make_field("deep", FieldType::Text)],
                    )],
                )],
            )],
        )];
        let def = make_collection_def("posts", fields, false);
        let names = get_column_names(&def);
        assert!(
            names.contains(&"outer__inner__deep".to_string()),
            "Group→Tabs→Group: outer__inner__deep"
        );
    }

    #[test]
    fn get_column_names_group_row_group_collapsible_four_levels() {
        let fields = vec![make_group_field(
            "a",
            vec![make_row_field(
                "r",
                vec![make_group_field(
                    "b",
                    vec![make_collapsible_field(
                        "c",
                        vec![make_field("leaf", FieldType::Text)],
                    )],
                )],
            )],
        )];
        let def = make_collection_def("posts", fields, false);
        let names = get_column_names(&def);
        assert!(
            names.contains(&"a__b__leaf".to_string()),
            "Group→Row→Group→Collapsible: a__b__leaf"
        );
    }

    #[test]
    fn get_valid_filter_columns_includes_expected() {
        let def = make_collection_def(
            "posts",
            vec![
                make_field("title", FieldType::Text),
                make_field("status", FieldType::Select),
                make_group_field("seo", vec![make_field("title", FieldType::Text)]),
            ],
            true,
        );
        let valid = get_valid_filter_columns(&def, None);
        assert!(valid.contains("id"));
        assert!(valid.contains("title"));
        assert!(valid.contains("status"));
        assert!(valid.contains("seo__title"));
        assert!(valid.contains("created_at"));
        assert!(valid.contains("updated_at"));
    }

    #[test]
    fn get_valid_filter_columns_excludes_array_and_blocks() {
        let def = make_collection_def(
            "posts",
            vec![
                make_field("title", FieldType::Text),
                make_field("tags", FieldType::Array),
                make_field("content", FieldType::Blocks),
            ],
            true,
        );
        let valid = get_valid_filter_columns(&def, None);
        assert!(valid.contains("title"), "Text fields should be included");
        assert!(!valid.contains("tags"), "Array fields should be excluded");
        assert!(
            !valid.contains("content"),
            "Blocks fields should be excluded"
        );
    }

    #[test]
    fn get_valid_filter_columns_group_containing_row() {
        let def = make_collection_def(
            "posts",
            vec![make_group_field(
                "meta",
                vec![make_row_field(
                    "r",
                    vec![make_field("title", FieldType::Text)],
                )],
            )],
            false,
        );
        let valid = get_valid_filter_columns(&def, None);
        assert!(
            valid.contains("meta__title"),
            "Group→Row filter: meta__title"
        );
    }

    #[test]
    fn get_valid_filter_columns_group_tabs_group() {
        let def = make_collection_def(
            "posts",
            vec![make_group_field(
                "outer",
                vec![make_tabs_field(
                    "t",
                    vec![FieldTab::new(
                        "Tab",
                        vec![make_group_field(
                            "inner",
                            vec![make_field("value", FieldType::Text)],
                        )],
                    )],
                )],
            )],
            false,
        );
        let valid = get_valid_filter_columns(&def, None);
        assert!(
            valid.contains("outer__inner__value"),
            "Group→Tabs→Group filter: outer__inner__value"
        );
    }
}
