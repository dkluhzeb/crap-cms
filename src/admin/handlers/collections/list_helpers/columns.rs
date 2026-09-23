//! List-table columns: the header row and the column picker.

use serde_json::{Value, json};

use super::access::{ListFieldAccess, field_label};
use crate::{
    admin::handlers::shared::{
        ListUrlContext, auto_label_from_name, is_column_eligible, is_meta_column,
        is_sortable_column,
    },
    core::collection::CollectionDefinition,
};

/// Keep only the column keys that exist and may be shown: a meta column this
/// collection actually has, or an eligible field the viewer is offered.
fn offered_columns(
    def: &CollectionDefinition,
    access: &ListFieldAccess,
    cols: &[String],
) -> Vec<String> {
    cols.iter()
        .filter(|k| {
            is_meta_column(k.as_str(), def)
                || def
                    .fields
                    .iter()
                    .any(|f| f.name == **k && is_column_eligible(&f.field_type) && access.offers(f))
        })
        .cloned()
        .collect()
}

/// The column keys to show, in order. Precedence: a per-user column selection
/// wins; then the collection's configured `admin.list_columns` default; then
/// the built-in fallback. The title field is never a column — it has its own.
fn column_keys(
    def: &CollectionDefinition,
    access: &ListFieldAccess,
    user_cols: Option<&[String]>,
) -> Vec<String> {
    let mut keys = if let Some(cols) = user_cols {
        offered_columns(def, access, cols)
    } else if !def.admin.list_columns.is_empty() {
        offered_columns(def, access, &def.admin.list_columns)
    } else {
        let mut defaults = Vec::new();

        if def.has_drafts() {
            defaults.push("_status".to_string());
        }

        defaults.push("created_at".to_string());
        defaults
    };

    if let Some(title) = def.title_field() {
        keys.retain(|k| k != title);
    }

    keys
}

/// A column's header label and whether it sorts.
fn column_label(def: &CollectionDefinition, key: &str) -> (String, bool) {
    match key {
        "created_at" => ("created".to_string(), true),
        "updated_at" => ("updated".to_string(), true),
        "_status" => ("status".to_string(), true),
        _ => match def.fields.iter().find(|f| f.name == key) {
            // Sortability must match `validate_sort` exactly — a has-many
            // relationship is a valid column but not sortable (no parent
            // column); a sort header for it would 400 on click.
            Some(f) => (field_label(f), is_sortable_column(key, def)),
            None => (auto_label_from_name(key), false),
        },
    }
}

/// Resolve which columns to display in the list table.
pub(in crate::admin::handlers::collections) fn resolve_columns(
    def: &CollectionDefinition,
    user_cols: Option<&[String]>,
    url_ctx: &ListUrlContext,
    access: &ListFieldAccess,
) -> Vec<Value> {
    let sort_field = url_ctx.sort.map(|s| s.strip_prefix('-').unwrap_or(s));
    let sort_desc = url_ctx.sort.is_some_and(|s| s.starts_with('-'));

    column_keys(def, access, user_cols)
        .iter()
        .map(|key| {
            let (label, sortable) = column_label(def, key);

            let is_sorted = sort_field == Some(key.as_str());
            let next_sort = if is_sorted && !sort_desc {
                format!("-{key}")
            } else {
                key.clone()
            };

            json!({
                "key": key,
                "label": label,
                "sortable": sortable,
                "sort_url": url_ctx.sort_url(&next_sort),
                "is_sorted_asc": is_sorted && !sort_desc,
                "is_sorted_desc": is_sorted && sort_desc,
            })
        })
        .collect()
}

/// The header label of the title column: the `use_as_title` field's label,
/// or `None` when the collection has no title field or the viewer is not
/// offered it (the column then shows the document id).
pub(in crate::admin::handlers::collections) fn title_label(
    def: &CollectionDefinition,
    access: &ListFieldAccess,
) -> Option<String> {
    let title = def.title_field()?;

    def.fields
        .iter()
        .find(|f| f.name == title && access.offers(f))
        .map(field_label)
}

/// Build the list of all eligible columns for the column picker UI.
pub(in crate::admin::handlers::collections) fn build_column_options(
    def: &CollectionDefinition,
    selected_keys: &[String],
    access: &ListFieldAccess,
) -> Vec<Value> {
    let option = |key: &str, label: String| {
        json!({
            "key": key,
            "label": label,
            "selected": selected_keys.iter().any(|k| k == key),
        })
    };

    let mut options = Vec::new();

    if def.has_drafts() {
        options.push(option("_status", "status".to_string()));
    }

    options.push(option("created_at", "created".to_string()));
    options.push(option("updated_at", "updated".to_string()));

    let title_field = def.title_field();

    let fields = def.fields.iter().filter(|f| {
        Some(f.name.as_str()) != title_field
            && is_column_eligible(&f.field_type)
            && access.offers(f)
    });
    for f in fields {
        options.push(option(&f.name, field_label(f)));
    }

    options
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::{
        admin::handlers::collections::list_helpers::test_helpers::{test_collection, test_url_ctx},
        core::{
            FieldAdmin, FieldDefinition, FieldType, LocalizedString, RelationshipConfig,
            VersionsConfig,
        },
    };

    fn open() -> ListFieldAccess {
        ListFieldAccess::default()
    }

    fn keys(cols: &[Value]) -> Vec<&str> {
        cols.iter().filter_map(|c| c["key"].as_str()).collect()
    }

    #[test]
    fn resolve_columns_defaults() {
        let def = test_collection();
        let cols = resolve_columns(&def, None, &test_url_ctx(None), &open());
        assert_eq!(cols.len(), 1);
        assert_eq!(cols[0]["key"], "created_at");
    }

    #[test]
    fn resolve_columns_user_cols() {
        let def = test_collection();
        let user_cols = vec!["status".to_string(), "views".to_string()];
        let cols = resolve_columns(&def, Some(&user_cols), &test_url_ctx(None), &open());
        assert_eq!(keys(&cols), vec!["status", "views"]);
    }

    /// Regression: a has-many relationship is a
    /// valid list column but is NOT sortable (no parent column) — its
    /// `sortable` flag must match `validate_sort`, or the rendered sort
    /// header 400s on click. A sortable scalar field stays sortable.
    #[test]
    fn has_many_relationship_column_is_not_sortable() {
        let mut def = test_collection();
        def.fields.push(
            FieldDefinition::builder("tags", FieldType::Relationship)
                .relationship(RelationshipConfig::new("tag", true)) // has_many
                .build(),
        );

        let user_cols = vec!["tags".to_string(), "views".to_string()];
        let cols = resolve_columns(&def, Some(&user_cols), &test_url_ctx(None), &open());

        let tags = cols
            .iter()
            .find(|c| c["key"] == "tags")
            .expect("tags column");
        assert_eq!(
            tags["sortable"],
            json!(false),
            "a has-many relationship column must not be sortable"
        );
        let views = cols
            .iter()
            .find(|c| c["key"] == "views")
            .expect("views column");
        assert_eq!(
            views["sortable"],
            json!(true),
            "a scalar number column stays sortable"
        );
    }

    /// Regression: `_status` is a column only on a collection that keeps
    /// drafts. Rendering the header elsewhere offered a sort link against a
    /// column the table never had.
    #[test]
    fn resolve_columns_drops_status_without_drafts() {
        let def = test_collection();
        let user_cols = vec!["_status".to_string(), "views".to_string()];

        let cols = resolve_columns(&def, Some(&user_cols), &test_url_ctx(None), &open());
        assert_eq!(keys(&cols), vec!["views"]);

        let mut with_drafts = test_collection();
        with_drafts.versions = Some(VersionsConfig::new(true, 10));

        let cols = resolve_columns(&with_drafts, Some(&user_cols), &test_url_ctx(None), &open());
        assert_eq!(keys(&cols), vec!["_status", "views"]);
    }

    #[test]
    fn resolve_columns_filters_invalid() {
        let def = test_collection();
        let user_cols = vec!["title".to_string(), "body".to_string(), "views".to_string()];
        let cols = resolve_columns(&def, Some(&user_cols), &test_url_ctx(None), &open());
        assert_eq!(keys(&cols), vec!["views"]);
    }

    /// With no per-user selection, the collection's configured
    /// `admin.list_columns` is used as the default (in order), overriding the
    /// built-in `created_at`-only fallback.
    #[test]
    fn resolve_columns_uses_collection_list_columns_default() {
        let mut def = test_collection();
        def.admin.list_columns = vec!["status".into(), "views".into(), "created_at".into()];

        let cols = resolve_columns(&def, None, &test_url_ctx(None), &open());
        assert_eq!(keys(&cols), vec!["status", "views", "created_at"]);
    }

    /// A per-user column selection wins over the collection's default.
    #[test]
    fn resolve_columns_user_selection_overrides_collection_default() {
        let mut def = test_collection();
        def.admin.list_columns = vec!["status".into()];
        let user_cols = vec!["views".to_string()];

        let cols = resolve_columns(&def, Some(&user_cols), &test_url_ctx(None), &open());
        assert_eq!(keys(&cols), vec!["views"]);
    }

    /// Regression: a `hidden` field, or one the viewer may not read, was
    /// offered as a column (and a sort header), so a saved selection or a
    /// configured default turned the whole list into a 403.
    #[test]
    fn resolve_columns_skips_hidden_and_unreadable_fields() {
        let mut def = test_collection();
        def.fields.push(
            FieldDefinition::builder("internal", FieldType::Text)
                .hidden(true)
                .build(),
        );
        def.admin.list_columns = vec!["internal".into(), "views".into(), "status".into()];
        let access = ListFieldAccess::new(HashSet::from(["views".to_string()]));

        let cols = resolve_columns(&def, None, &test_url_ctx(None), &access);
        assert_eq!(keys(&cols), vec!["status"]);

        let user_cols = vec!["internal".to_string(), "views".to_string()];
        let cols = resolve_columns(&def, Some(&user_cols), &test_url_ctx(None), &access);
        assert!(cols.is_empty());
    }

    #[test]
    fn resolve_columns_sort_state() {
        let def = test_collection();
        let user_cols = vec!["views".to_string()];
        let cols = resolve_columns(
            &def,
            Some(&user_cols),
            &test_url_ctx(Some("views")),
            &open(),
        );
        assert_eq!(cols[0]["is_sorted_asc"], true);
        assert_eq!(cols[0]["is_sorted_desc"], false);
    }

    #[test]
    fn resolve_columns_sort_desc_state() {
        let def = test_collection();
        let user_cols = vec!["views".to_string()];
        let cols = resolve_columns(
            &def,
            Some(&user_cols),
            &test_url_ctx(Some("-views")),
            &open(),
        );
        assert_eq!(cols[0]["is_sorted_asc"], false);
        assert_eq!(cols[0]["is_sorted_desc"], true);
    }

    #[test]
    fn build_column_options_includes_eligible() {
        let def = test_collection();
        let opts = build_column_options(&def, &["status".to_string()], &open());
        let keys = keys(&opts);
        assert!(keys.contains(&"created_at"));
        assert!(keys.contains(&"updated_at"));
        assert!(keys.contains(&"status")); // select - eligible
        assert!(keys.contains(&"views")); // number - eligible
        assert!(!keys.contains(&"body")); // richtext - ineligible
        assert!(!keys.contains(&"title")); // title field - excluded
    }

    #[test]
    fn build_column_options_marks_selected() {
        let def = test_collection();
        let opts = build_column_options(&def, &["status".to_string()], &open());
        let status_opt = opts.iter().find(|o| o["key"] == "status").unwrap();
        assert_eq!(status_opt["selected"], true);
        let views_opt = opts.iter().find(|o| o["key"] == "views").unwrap();
        assert_eq!(views_opt["selected"], false);
    }

    #[test]
    fn build_column_options_skips_hidden_and_unreadable_fields() {
        let mut def = test_collection();
        def.fields.push(
            FieldDefinition::builder("internal", FieldType::Text)
                .hidden(true)
                .build(),
        );
        let access = ListFieldAccess::new(HashSet::from(["views".to_string()]));

        let opts = build_column_options(&def, &[], &access);
        let keys = keys(&opts);
        assert!(!keys.contains(&"internal"));
        assert!(!keys.contains(&"views"));
        assert!(keys.contains(&"status"));
    }

    /// Regression: the title column header rendered the raw field name. It
    /// shows the field's label, and nothing (the id header) when the viewer
    /// is not offered the title field.
    #[test]
    fn title_label_is_the_title_fields_label() {
        let mut def = test_collection();
        def.fields[0] = FieldDefinition::builder("title", FieldType::Text)
            .admin(
                FieldAdmin::builder()
                    .label(LocalizedString::Plain("Headline".into()))
                    .build(),
            )
            .build();

        assert_eq!(title_label(&def, &open()).as_deref(), Some("Headline"));

        let denied = ListFieldAccess::new(HashSet::from(["title".to_string()]));
        assert_eq!(title_label(&def, &denied), None);

        def.admin.use_as_title = None;
        assert_eq!(title_label(&def, &open()), None);
    }
}
