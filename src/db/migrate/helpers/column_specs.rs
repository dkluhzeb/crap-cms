//! Column specification collection from field definitions.

use anyhow::{Context as _, Result};
use tracing::info;

use crate::{
    config::LocaleConfig,
    core::FieldDefinition,
    db::{
        DbConnection,
        migrate::collection::append_default_value_for,
        query::helpers::{prefixed_name, quote_ident, walk_leaf_fields},
    },
};

use super::introspection::get_table_columns;

/// A column specification derived from a field definition.
/// Used by migration code to generate CREATE TABLE / ALTER TABLE statements.
pub(in crate::db::migrate) struct ColumnSpec<'a> {
    /// The column name (e.g., "title", "`social__github`")
    pub col_name: String,
    /// The field definition this column comes from (used for type, constraints)
    pub field: &'a FieldDefinition,
    /// Whether this column is localized (needs per-locale columns)
    pub is_localized: bool,
    /// Companion column (`_tz` / `_lang`). Always TEXT, no constraints.
    pub companion_text: bool,
}

impl ColumnSpec<'_> {
    /// The backend DDL type for this column — the single source of truth shared
    /// by the CREATE TABLE and ALTER TABLE (reconcile) paths, so the two can't
    /// disagree on a column's type.
    ///
    /// Companion columns (`_tz` / `_lang`) and **scalar has-many lists** (stored
    /// as a JSON array, see [`FieldDefinition::is_has_many_scalar`]) are always
    /// `TEXT`; everything else uses the backend's per-field-type mapping.
    pub(in crate::db::migrate) fn ddl_type(&self, conn: &dyn DbConnection) -> &'static str {
        if self.companion_text || self.field.is_has_many_scalar() {
            "TEXT"
        } else {
            conn.column_type_for(&self.field.field_type)
        }
    }

    /// The full column definition of `col_name` (the spec's column, or one of
    /// its locale columns): the quoted name, the [`Self::ddl_type`], and the
    /// field's DEFAULT — the one definition every CREATE TABLE and ALTER TABLE
    /// ADD COLUMN of a collection or global uses.
    ///
    /// It never carries `NOT NULL`: `required` is enforced by validation on
    /// every write surface, which also knows when it does not apply (a draft
    /// save, a non-default locale). A constraint baked into the table would
    /// outlive the definition that asked for it — removing `required`,
    /// enabling drafts or removing the field would leave every write that
    /// omits the value failing at the database. A companion column carries no
    /// default either.
    pub(in crate::db::migrate) fn column_def(
        &self,
        conn: &dyn DbConnection,
        col_name: &str,
    ) -> String {
        let mut col = format!("{} {}", quote_ident(col_name), self.ddl_type(conn));

        if !self.companion_text {
            append_default_value_for(&mut col, self.field);
        }

        col
    }
}

/// Collect column specifications from a field tree.
/// Uses `walk_leaf_fields` to handle Group/Row/Collapsible/Tabs recursion.
pub(in crate::db::migrate) fn collect_column_specs<'a>(
    fields: &'a [FieldDefinition],
    locale_config: &LocaleConfig,
) -> Vec<ColumnSpec<'a>> {
    let mut specs = Vec::new();

    // walk_leaf_fields is infallible here — the closure never errors.
    let _ = walk_leaf_fields(
        fields,
        "",
        false,
        &mut |field: &'a FieldDefinition, prefix, inherited_localized| {
            if !field.has_parent_column() {
                return Ok(());
            }

            let col_name = prefixed_name(prefix, &field.name);

            let is_localized =
                (inherited_localized || field.localized) && locale_config.is_enabled();

            specs.push(ColumnSpec {
                col_name: col_name.clone(),
                field,
                is_localized,
                companion_text: false,
            });

            for companion in field.companion_columns(&col_name) {
                specs.push(ColumnSpec {
                    col_name: companion,
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

/// The `_locale` column definition of a junction table. Rows written before the
/// column existed belong to the default locale, stored as its code — the value
/// every read and write filters on, not the column form of the code.
pub(in crate::db::migrate) fn locale_column_definition(default_locale: &str) -> String {
    format!(
        "_locale TEXT NOT NULL DEFAULT '{}'",
        default_locale.replace('\'', "''")
    )
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
            "ALTER TABLE \"{}\" ADD COLUMN {}",
            table_name,
            locale_column_definition(default_locale)
        );
        info!("Adding _locale column to {}", table_name);
        conn.execute_ddl(&sql, &[])
            .with_context(|| format!("Failed to add _locale to {table_name}"))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{FieldAdmin, FieldDefinition, FieldTab, FieldType};
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
    fn code_with_languages_produces_companion_lang_column() {
        let fields = vec![
            FieldDefinition::builder("snippet", FieldType::Code)
                .admin(
                    FieldAdmin::builder()
                        .languages(vec!["javascript".to_string(), "python".to_string()])
                        .build(),
                )
                .build(),
        ];
        let specs = collect_column_specs(&fields, &no_locale());
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].col_name, "snippet");
        assert!(!specs[0].companion_text);
        assert_eq!(specs[1].col_name, "snippet_lang");
        assert!(specs[1].companion_text);
    }

    #[test]
    fn code_without_languages_produces_one_spec() {
        let fields = vec![
            FieldDefinition::builder("snippet", FieldType::Code)
                .admin(FieldAdmin::builder().language("javascript").build())
                .build(),
        ];
        let specs = collect_column_specs(&fields, &no_locale());
        assert_eq!(specs.len(), 1, "no companion column without an allow-list");
        assert_eq!(specs[0].col_name, "snippet");
    }

    #[test]
    fn code_languages_in_group_produces_prefixed_lang_column() {
        let fields = vec![
            FieldDefinition::builder("meta", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("example", FieldType::Code)
                        .admin(
                            FieldAdmin::builder()
                                .languages(vec!["javascript".to_string()])
                                .build(),
                        )
                        .build(),
                ])
                .build(),
        ];
        let specs = collect_column_specs(&fields, &no_locale());
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].col_name, "meta__example");
        assert_eq!(specs[1].col_name, "meta__example_lang");
        assert!(specs[1].companion_text);
    }

    /// Regression: the `_locale` default was the column form of the code
    /// (`en_US`) while reads and writes filter on the code itself (`en-US`), so
    /// rows present before a field became localized vanished from every read.
    #[test]
    fn rows_before_localization_belong_to_the_default_locale_code() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        conn.execute(
            "CREATE TABLE test_join (parent_id TEXT, related_id TEXT)",
            &[],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO test_join (parent_id, related_id) VALUES ('p1', 'r1')",
            &[],
        )
        .unwrap();

        ensure_locale_column(&conn, "test_join", "en-US").unwrap();

        let row = conn
            .query_one("SELECT _locale FROM test_join", &[])
            .unwrap()
            .unwrap();
        assert_eq!(row.get_string("_locale").unwrap(), "en-US");
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
