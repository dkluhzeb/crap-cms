//! Field/column resolution helpers for FTS5 indexing.

use std::collections::HashMap;

use anyhow::{Result, bail};

use crate::{
    config::LocaleConfig,
    core::{
        CollectionDefinition, FieldDefinition, FieldDenial, FieldType, Registry,
        richtext::SearchableAttrs,
    },
    db::query::{
        fts::layout::fts_columns,
        helpers::{prefixed_name, walk_leaf_fields},
    },
    hooks::lifecycle::access::collect_denials_flat,
};

/// Text-like field types eligible for default FTS indexing.
fn is_text_like(field_type: &FieldType) -> bool {
    matches!(
        field_type,
        FieldType::Text
            | FieldType::Textarea
            | FieldType::Richtext
            | FieldType::Email
            | FieldType::Code
    )
}

/// Determine which logical fields should be indexed in the FTS5 table.
///
/// Uses `list_searchable_fields` if configured, otherwise falls back to all
/// text-like fields (Text, Textarea, Richtext, Email, Code) — including those
/// nested in groups (collected as `group__field` columns) and promoted through
/// layout wrappers. Array/Blocks sub-fields are excluded (they live in join
/// tables, not on the parent row).
///
/// A configured list is refused at definition time when an entry is not a
/// searchable field ([`validate_searchable_fields`]); the same rule filters it
/// here, so a definition built in code without that check still never indexes
/// a column the index cannot read.
#[must_use]
pub fn get_fts_fields(def: &CollectionDefinition) -> Vec<String> {
    if !def.admin.list_searchable_fields.is_empty() {
        return def
            .admin
            .list_searchable_fields
            .iter()
            .filter(|name| searchable_field_problem(name, &def.fields).is_none())
            .cloned()
            .collect();
    }

    collect_fts_defaults(&def.fields)
}

/// Refuse a collection whose `admin.list_searchable_fields` names anything but
/// a searchable field on the document row — an unknown name (a typo), an
/// array/blocks/group, a hidden field, or a field whose type holds no
/// searchable text (see [`FieldType::is_searchable`]).
///
/// Dropping such an entry silently would search fewer fields than configured,
/// and a list with no valid entry would build no index at all — every search
/// would then return the whole collection.
///
/// # Errors
///
/// Returns an error naming the collection, the entry and why it cannot be
/// searched.
pub fn validate_searchable_fields(def: &CollectionDefinition) -> Result<()> {
    for name in &def.admin.list_searchable_fields {
        if let Some(problem) = searchable_field_problem(name, &def.fields) {
            bail!(
                "Collection '{}': admin.list_searchable_fields entry '{name}' {problem}",
                def.slug
            );
        }
    }

    Ok(())
}

/// Why the flat column name `name` cannot be searched, or `None` when it can.
/// Resolves names as FTS indexes them (`walk_leaf_fields`): top-level and
/// wrapper-promoted fields by bare name, group sub-fields as `group__field`.
fn searchable_field_problem(name: &str, fields: &[FieldDefinition]) -> Option<String> {
    let Some(field_type) = leaf_field_type(name, fields) else {
        return Some(
            "is not a field on the document row (name a group sub-field as \
             `group__field`; array and blocks sub-fields cannot be searched)"
                .to_string(),
        );
    };

    if !field_type.is_searchable() {
        return Some(format!(
            "has type '{}'; only text, textarea, richtext, email, code, select and radio \
             fields can be searched",
            field_type.as_str()
        ));
    }

    if covered_by(&column_prefixes(fields, &|f| f.hidden), name) {
        return Some(
            "is hidden (or inside a hidden group); hidden fields are never indexed, \
             since a search hit would reveal the value"
                .to_string(),
        );
    }

    None
}

/// The type of the field whose flat column name is `name`.
fn leaf_field_type(name: &str, fields: &[FieldDefinition]) -> Option<FieldType> {
    let mut found = None;

    let _ = walk_leaf_fields(fields, "", false, &mut |field, prefix, _| {
        if prefixed_name(prefix, &field.name) == name {
            found = Some(field.field_type.clone());
        }
        Ok(())
    });

    found
}

/// Flat column names (and `group__` prefixes) whose field — or an ancestor —
/// matches `is_denied`. Shares the field-access denial walker so "beneath a
/// denied parent" means the same thing here as in the read strip.
fn column_prefixes(
    fields: &[FieldDefinition],
    is_denied: &impl Fn(&FieldDefinition) -> bool,
) -> Vec<String> {
    let mut denials = Vec::new();
    collect_denials_flat(fields, is_denied, "", &mut denials);

    denials.iter().map(FieldDenial::display_path).collect()
}

/// Whether `name` is one of `prefixes` or a `group__child` beneath one.
fn covered_by(prefixes: &[String], name: &str) -> bool {
    prefixes.iter().any(|p| {
        name == p
            || name
                .strip_prefix(p.as_str())
                .is_some_and(|rest| rest.starts_with("__"))
    })
}

/// Collect default FTS fields (text-like) — top level, group sub-fields (as
/// `group__field` columns), and layout-wrapper-promoted children. Array/Blocks
/// sub-fields are excluded (they live in join tables, visited here as opaque
/// leaf columns and filtered out by the text-like check).
///
/// API-hidden fields and fields with an `access.read` rule are never indexed
/// by default: the index is shared by every reader, so a search hit on such a
/// field would leak what the read strip removes. An operator can still list a
/// read-gated field in `list_searchable_fields` explicitly.
fn collect_fts_defaults(fields: &[FieldDefinition]) -> Vec<String> {
    let guarded = column_prefixes(fields, &|f| f.hidden || f.access.read.is_some());

    let mut result = Vec::new();
    let _ = walk_leaf_fields(fields, "", false, &mut |field, prefix, _| {
        let name = prefixed_name(prefix, &field.name);
        if is_text_like(&field.field_type) && !covered_by(&guarded, &name) {
            result.push(name);
        }
        Ok(())
    });
    result
}

/// Expand logical field names to actual database column names.
///
/// For non-localized fields, the column name is the field name.
/// For localized fields, each field expands to `field__locale` for each locale.
///
/// # Errors
///
/// Returns an error if any field name is not a plain identifier or conflicts
/// with locale-suffixed naming.
pub fn get_fts_columns(
    def: &CollectionDefinition,
    locale_config: &LocaleConfig,
) -> Result<Vec<String>> {
    Ok(fts_columns(def, locale_config)?
        .into_iter()
        .map(|column| column.name)
        .collect())
}

/// How an indexed rich text column's value is read for its text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RichtextFormat {
    Html,
    Json,
}

/// The rich text columns of `def` and their storage format — including
/// group-nested ones (as `group__field`), so a rich text field indexed by FTS
/// is indexed by its text rather than its markup wherever it lives.
pub(super) fn richtext_columns(def: &CollectionDefinition) -> HashMap<String, RichtextFormat> {
    let mut map = HashMap::new();
    let _ = walk_leaf_fields(&def.fields, "", false, &mut |field, prefix, _| {
        if field.field_type == FieldType::Richtext {
            let format = if field.parses_json() {
                RichtextFormat::Json
            } else {
                RichtextFormat::Html
            };
            map.insert(prefixed_name(prefix, &field.name), format);
        }
        Ok(())
    });
    map
}

/// The rich text format of an FTS column — a rich text column name in
/// `rt_cols`, or its per-locale form `column__locale`. The locale is the
/// trailing `__` segment, so it's stripped from the tail (`seo__body__en` →
/// `seo__body`), never the head. `None` for any other column.
pub(super) fn richtext_column_format(
    col_name: &str,
    rt_cols: &HashMap<String, RichtextFormat>,
) -> Option<RichtextFormat> {
    rt_cols.get(col_name).copied().or_else(|| {
        col_name
            .rsplit_once("__")
            .and_then(|(base, _locale)| rt_cols.get(base).copied())
    })
}

/// Build a map of node type name → searchable attr names from collection definition
/// and registry. Used for FTS extraction of custom richtext node content, in
/// either storage format.
pub(super) fn build_node_searchable_map<'a>(
    def: &'a CollectionDefinition,
    registry: Option<&'a Registry>,
) -> SearchableAttrs<'a> {
    let mut map = SearchableAttrs::new();
    let Some(registry) = registry else {
        return map;
    };
    // Descend groups so a richtext field nested in a group contributes its
    // searchable node attrs too (the map is keyed by node type, so the column
    // prefix is irrelevant here — only reaching every richtext field matters).
    let _ = walk_leaf_fields(&def.fields, "", false, &mut |field, _prefix, _| {
        if field.field_type != FieldType::Richtext {
            return Ok(());
        }

        for node_name in &field.admin.nodes {
            if let Some(node_def) = registry.get_richtext_node(node_name)
                && !node_def.searchable_attrs.is_empty()
            {
                map.insert(
                    node_def.name.as_str(),
                    node_def
                        .searchable_attrs
                        .iter()
                        .map(String::as_str)
                        .collect(),
                );
            }
        }
        Ok(())
    });
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{HookRef, collection::*, field::*};
    use crate::db::migrate::collection::test_helpers::{locale_en_de, localized_field, text_field};

    /// Hidden and read-gated fields never enter the default index; a hidden
    /// field is dropped even when listed explicitly, a read-gated one is kept
    /// when the operator lists it.
    #[test]
    fn default_fts_fields_skip_hidden_and_read_gated() {
        let mut def = CollectionDefinition::new("vault");
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("secret", FieldType::Text)
                .hidden(true)
                .build(),
            FieldDefinition::builder("notes", FieldType::Textarea)
                .access(FieldAccess {
                    read: Some(HookRef::new("access.admin_only")),
                    ..Default::default()
                })
                .build(),
        ];

        assert_eq!(get_fts_fields(&def), vec!["title".to_string()]);

        def.admin.list_searchable_fields = vec!["title".into(), "secret".into(), "notes".into()];
        assert_eq!(
            get_fts_fields(&def),
            vec!["title".to_string(), "notes".to_string()]
        );
    }

    /// A hidden group hides every `group__child` column beneath it.
    #[test]
    fn default_fts_fields_skip_children_of_a_hidden_group() {
        let mut def = CollectionDefinition::new("vault");
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("internal", FieldType::Group)
                .hidden(true)
                .fields(vec![
                    FieldDefinition::builder("memo", FieldType::Text).build(),
                ])
                .build(),
        ];

        assert_eq!(get_fts_fields(&def), vec!["title".to_string()]);
    }
    fn simple_def(fields: Vec<FieldDefinition>) -> CollectionDefinition {
        let mut def = CollectionDefinition::new("posts");
        def.fields = fields;
        def
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
    fn get_fts_fields_includes_group_subfields_excludes_array() {
        let def = simple_def(vec![
            text_field("title"),
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![text_field("label")])
                .build(),
            FieldDefinition::builder("meta", FieldType::Group)
                .fields(vec![text_field("description")])
                .build(),
        ]);
        // Group sub-fields ARE parent-table columns (`meta__description`) and are
        // indexed; Array sub-fields live in a join table and are not.
        assert_eq!(get_fts_fields(&def), vec!["title", "meta__description"]);
    }

    #[test]
    fn default_fts_includes_group_nested_text() {
        // Regression: a text field inside a group must be FTS-default as its real
        // `group__field` column — previously groups were skipped entirely.
        let def = simple_def(vec![
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![text_field("title"), text_field("description")])
                .build(),
        ]);
        assert_eq!(get_fts_fields(&def), vec!["seo__title", "seo__description"]);
    }

    #[test]
    fn searchable_accepts_group_subfield_by_prefixed_name() {
        // A user can now list a group sub-field as `seo__title` in
        // list_searchable_fields and have it resolve.
        let mut def = simple_def(vec![
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![text_field("title"), text_field("desc")])
                .build(),
        ]);
        def.admin.list_searchable_fields = vec!["seo__title".into()];
        assert_eq!(get_fts_fields(&def), vec!["seo__title"]);
    }

    #[test]
    fn get_fts_columns_expands_localized_group_subfield() {
        // A localized text field inside a group expands per-locale as
        // `seo__title__en` / `seo__title__de`.
        let def = simple_def(vec![
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![localized_field("title")])
                .build(),
        ]);
        let cols = get_fts_columns(&def, &locale_en_de()).unwrap();
        assert_eq!(cols, vec!["seo__title__en", "seo__title__de"]);
    }

    #[test]
    fn richtext_columns_include_group_nested_in_both_formats() {
        let mut body = FieldDefinition::builder("body", FieldType::Richtext).build();
        body.admin.richtext_format = Some("json".into());
        let def = simple_def(vec![
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![body])
                .build(),
            FieldDefinition::builder("intro", FieldType::Richtext).build(),
        ]);
        let cols = richtext_columns(&def);

        assert_eq!(
            cols.get("seo__body"),
            Some(&RichtextFormat::Json),
            "{cols:?}"
        );
        assert_eq!(cols.get("intro"), Some(&RichtextFormat::Html), "{cols:?}");
    }

    #[test]
    fn richtext_column_format_handles_group_prefix_and_locale_suffix() {
        let cols: HashMap<String, RichtextFormat> = [
            ("seo__body".to_string(), RichtextFormat::Json),
            ("body".to_string(), RichtextFormat::Html),
        ]
        .into_iter()
        .collect();
        // group, non-localized + localized; top-level non-localized + localized
        assert_eq!(
            richtext_column_format("seo__body", &cols),
            Some(RichtextFormat::Json)
        );
        assert_eq!(
            richtext_column_format("seo__body__en", &cols),
            Some(RichtextFormat::Json)
        );
        assert_eq!(
            richtext_column_format("body", &cols),
            Some(RichtextFormat::Html)
        );
        assert_eq!(
            richtext_column_format("body__de", &cols),
            Some(RichtextFormat::Html)
        );
        // a plain text group column must NOT be mis-detected (locale strip is
        // tail-only, so `seo__title` does not resolve to the group root `seo`)
        assert_eq!(richtext_column_format("seo__title", &cols), None);
        assert_eq!(richtext_column_format("seo", &cols), None);
    }

    #[test]
    fn get_fts_columns_no_locale_returns_field_names() {
        let def = simple_def(vec![text_field("title"), text_field("body")]);
        let cols = get_fts_columns(&def, &LocaleConfig::default()).unwrap();
        assert_eq!(cols, vec!["title", "body"]);
    }

    #[test]
    fn get_fts_columns_with_locale_expands_localized_fields() {
        let def = simple_def(vec![localized_field("title"), localized_field("body")]);
        let cols = get_fts_columns(&def, &locale_en_de()).unwrap();
        assert_eq!(cols, vec!["title__en", "title__de", "body__en", "body__de"]);
    }

    #[test]
    fn get_fts_columns_mixed_localized_and_non_localized() {
        let def = simple_def(vec![localized_field("title"), text_field("slug")]);
        let cols = get_fts_columns(&def, &locale_en_de()).unwrap();
        assert_eq!(cols, vec!["title__en", "title__de", "slug"]);
    }

    #[test]
    fn get_fts_columns_locale_enabled_but_no_localized_fields() {
        let def = simple_def(vec![text_field("title"), text_field("body")]);
        let cols = get_fts_columns(&def, &locale_en_de()).unwrap();
        // None of the fields are localized, so no expansion
        assert_eq!(cols, vec!["title", "body"]);
    }

    #[test]
    fn get_fts_columns_empty_when_no_text_fields() {
        let def = simple_def(vec![
            FieldDefinition::builder("count", FieldType::Number).build(),
        ]);
        let cols = get_fts_columns(&def, &locale_en_de()).unwrap();
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
        // A name that doesn't match any field definition is refused at
        // definition time (`validate_searchable_fields`); a definition built in
        // code without that check still never indexes it.
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

    fn problem(def: &CollectionDefinition) -> String {
        validate_searchable_fields(def)
            .expect_err("refused")
            .to_string()
    }

    /// Regression: a typo or a container name in `list_searchable_fields` was
    /// only warned about and dropped — a list with no valid entry built no
    /// index, so every search returned the whole collection. It is now a load
    /// error naming the entry.
    #[test]
    fn unknown_and_container_entries_are_refused() {
        let mut def = simple_def(vec![
            text_field("title"),
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![text_field("label")])
                .build(),
        ]);

        def.admin.list_searchable_fields = vec!["titel".into()];
        let err = problem(&def);
        assert!(
            err.contains("'titel'") && err.contains("not a field"),
            "{err}"
        );

        def.admin.list_searchable_fields = vec!["items".into()];
        let err = problem(&def);
        assert!(err.contains("'items'") && err.contains("array"), "{err}");

        def.admin.list_searchable_fields = vec!["items__label".into()];
        assert!(problem(&def).contains("not a field"));
    }

    /// Regression: a number (or checkbox) entry indexed `COALESCE(col, '')`,
    /// which Postgres refuses for a numeric column — the server would not boot.
    /// Every non-text type is refused at definition time on both backends.
    #[test]
    fn non_text_entries_are_refused() {
        let mut def = simple_def(vec![
            FieldDefinition::builder("price", FieldType::Number).build(),
            FieldDefinition::builder("active", FieldType::Checkbox).build(),
            FieldDefinition::builder("published", FieldType::Date).build(),
        ]);

        for name in ["price", "active", "published"] {
            def.admin.list_searchable_fields = vec![name.into()];
            let err = problem(&def);

            assert!(
                err.contains(&format!("'{name}'")) && err.contains("has type"),
                "{err}"
            );
        }
    }

    #[test]
    fn hidden_entries_are_refused() {
        let mut def = simple_def(vec![
            FieldDefinition::builder("secret", FieldType::Text)
                .hidden(true)
                .build(),
        ]);
        def.admin.list_searchable_fields = vec!["secret".into()];

        assert!(problem(&def).contains("hidden"));
    }

    #[test]
    fn text_bearing_entries_are_accepted() {
        let mut def = simple_def(vec![
            text_field("title"),
            FieldDefinition::builder("status", FieldType::Select).build(),
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![text_field("title")])
                .build(),
        ]);
        def.admin.list_searchable_fields =
            vec!["title".into(), "status".into(), "seo__title".into()];

        validate_searchable_fields(&def).expect("valid");
        assert_eq!(get_fts_fields(&def), vec!["title", "status", "seo__title"]);
    }

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

    #[test]
    fn get_fts_columns_localized_field_inside_wrapper_expands() {
        // Regression: a localized text field promoted through a Row must expand
        // to `field__locale` columns, not be mis-resolved to a bare column.
        let def = simple_def(vec![FieldDefinition {
            name: "row".to_string(),
            field_type: FieldType::Row,
            fields: vec![localized_field("title")],
            ..Default::default()
        }]);

        let cols = get_fts_columns(&def, &locale_en_de()).unwrap();
        assert_eq!(cols, vec!["title__en", "title__de"]);
    }
}
