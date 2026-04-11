//! Column specification collection from field definitions.

use anyhow::{Context as _, Result};
use tracing::info;

use crate::{
    config::LocaleConfig,
    core::{FieldDefinition, FieldType},
    db::{
        DbConnection,
        query::helpers::{prefixed_name, tz_column, walk_leaf_fields},
    },
};

use super::introspection::{get_table_columns, sanitize_locale};

/// A column specification derived from a field definition.
/// Used by migration code to generate CREATE TABLE / ALTER TABLE statements.
pub(in crate::db::migrate) struct ColumnSpec<'a> {
    /// The column name (e.g., "title", "social__github")
    pub col_name: String,
    /// The field definition this column comes from (used for type, constraints)
    pub field: &'a FieldDefinition,
    /// Whether this column is localized (needs per-locale columns)
    pub is_localized: bool,
    /// Companion column (e.g., timezone). Always TEXT, no constraints.
    pub companion_text: bool,
}

/// Collect column specifications from a field tree.
/// Uses `walk_leaf_fields` to handle Group/Row/Collapsible/Tabs recursion.
pub(in crate::db::migrate) fn collect_column_specs<'a>(
    fields: &'a [FieldDefinition],
    locale_config: &LocaleConfig,
) -> Vec<ColumnSpec<'a>> {
    let mut specs = Vec::new();

    // walk_leaf_fields is infallible here — the closure never errors.
    // SAFETY: walk_leaf_fields only passes references derived from its `fields`
    // parameter, which has lifetime `'a`. The closure's anonymous field lifetime
    // is actually `'a`, but the compiler can't prove it through the mutable
    // closure boundary (invariance of `&mut Vec<ColumnSpec<'a>>`). We re-derive
    // the `'a` lifetime via a pointer round-trip.
    let _ = walk_leaf_fields(
        fields,
        "",
        false,
        &mut |field, prefix, inherited_localized| {
            if !field.has_parent_column() {
                return Ok(());
            }

            let field: &'a FieldDefinition = unsafe { &*(field as *const FieldDefinition) };
            let col_name = prefixed_name(prefix, &field.name);

            let is_localized =
                (inherited_localized || field.localized) && locale_config.is_enabled();

            specs.push(ColumnSpec {
                col_name: col_name.clone(),
                field,
                is_localized,
                companion_text: false,
            });

            if field.field_type == FieldType::Date && field.timezone {
                specs.push(ColumnSpec {
                    col_name: tz_column(&col_name),
                    field,
                    is_localized,
                    companion_text: true,
                });
            }

            Ok(())
        },
    );

    specs
}

/// Ensure a `_locale` column exists on a junction table (for ALTER TABLE on existing tables).
pub(in crate::db::migrate) fn ensure_locale_column(
    conn: &dyn DbConnection,
    table_name: &str,
    default_locale: &str,
) -> Result<()> {
    let existing = get_table_columns(conn, table_name)?;

    if !existing.contains("_locale") {
        let sql = format!(
            "ALTER TABLE \"{}\" ADD COLUMN _locale TEXT NOT NULL DEFAULT '{}'",
            table_name,
            sanitize_locale(default_locale)?
        );
        info!("Adding _locale column to {}", table_name);
        conn.execute_ddl(&sql, &[])
            .with_context(|| format!("Failed to add _locale to {}", table_name))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::field::{FieldDefinition, FieldTab, FieldType};
    use crate::db::migrate::collection::test_helpers::*;

    #[test]
    fn group_containing_row() {
        let fields = vec![
            FieldDefinition::builder("meta", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("r", FieldType::Row)
                        .fields(vec![text_field("title"), text_field("slug")])
                        .build(),
                ])
                .build(),
        ];
        let specs = collect_column_specs(&fields, &no_locale());
        let names: Vec<&str> = specs.iter().map(|s| s.col_name.as_str()).collect();
        assert!(names.contains(&"meta__title"));
        assert!(names.contains(&"meta__slug"));
    }

    #[test]
    fn group_containing_tabs() {
        let fields = vec![
            FieldDefinition::builder("settings", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("t", FieldType::Tabs)
                        .tabs(vec![
                            FieldTab::new("General", vec![text_field("theme")]),
                            FieldTab::new("Advanced", vec![text_field("cache_ttl")]),
                        ])
                        .build(),
                ])
                .build(),
        ];
        let specs = collect_column_specs(&fields, &no_locale());
        let names: Vec<&str> = specs.iter().map(|s| s.col_name.as_str()).collect();
        assert!(names.contains(&"settings__theme"));
        assert!(names.contains(&"settings__cache_ttl"));
    }

    #[test]
    fn group_tabs_group_three_levels() {
        let fields = vec![
            FieldDefinition::builder("outer", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("t", FieldType::Tabs)
                        .tabs(vec![FieldTab::new(
                            "Tab",
                            vec![
                                FieldDefinition::builder("inner", FieldType::Group)
                                    .fields(vec![text_field("deep")])
                                    .build(),
                            ],
                        )])
                        .build(),
                ])
                .build(),
        ];
        let specs = collect_column_specs(&fields, &no_locale());
        let names: Vec<&str> = specs.iter().map(|s| s.col_name.as_str()).collect();
        assert!(names.contains(&"outer__inner__deep"));
    }

    #[test]
    fn localized_group_tabs_inherits_locale() {
        let fields = vec![
            FieldDefinition::builder("meta", FieldType::Group)
                .localized(true)
                .fields(vec![
                    FieldDefinition::builder("t", FieldType::Tabs)
                        .tabs(vec![FieldTab::new("Content", vec![text_field("title")])])
                        .build(),
                ])
                .build(),
        ];
        let specs = collect_column_specs(&fields, &locale_en_de());
        assert!(
            specs
                .iter()
                .any(|s| s.col_name == "meta__title" && s.is_localized)
        );
    }

    #[test]
    fn date_with_timezone_produces_two_specs() {
        let fields = vec![
            FieldDefinition::builder("event_at", FieldType::Date)
                .timezone(true)
                .build(),
        ];
        let specs = collect_column_specs(&fields, &no_locale());
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].col_name, "event_at");
        assert!(!specs[0].companion_text);
        assert_eq!(specs[1].col_name, "event_at_tz");
        assert!(specs[1].companion_text);
    }

    #[test]
    fn date_without_timezone_produces_one_spec() {
        let fields = vec![FieldDefinition::builder("published_at", FieldType::Date).build()];
        let specs = collect_column_specs(&fields, &no_locale());
        assert_eq!(specs.len(), 1);
        assert!(!specs[0].companion_text);
    }

    #[test]
    fn date_timezone_in_group_produces_prefixed_tz() {
        let fields = vec![
            FieldDefinition::builder("meta", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("starts_at", FieldType::Date)
                        .timezone(true)
                        .build(),
                ])
                .build(),
        ];
        let specs = collect_column_specs(&fields, &no_locale());
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].col_name, "meta__starts_at");
        assert_eq!(specs[1].col_name, "meta__starts_at_tz");
        assert!(specs[1].companion_text);
    }

    #[test]
    fn ensure_locale_column_adds_to_existing() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        conn.execute(
            "CREATE TABLE test_join (parent_id TEXT, related_id TEXT)",
            &[],
        )
        .unwrap();
        ensure_locale_column(&conn, "test_join", "en").unwrap();

        let cols = super::get_table_columns(&conn, "test_join").unwrap();
        assert!(cols.contains("_locale"));

        // Idempotent
        ensure_locale_column(&conn, "test_join", "en").unwrap();
    }
}
