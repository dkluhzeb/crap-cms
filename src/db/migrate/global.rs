//! Global table sync: create and alter global tables from Lua definitions.

use anyhow::{Context as _, Result};
use std::collections::{HashMap, HashSet};
use tracing::info;

use crate::{
    config::LocaleConfig,
    core::collection::GlobalDefinition,
    db::{
        DbConnection,
        query::helpers::{global_table, locale_column, quote_ident},
    },
};

use crate::db::migrate::{
    collection::append_default_value_for,
    helpers::{
        ColumnSpec, add_column_if_missing, collect_column_specs, get_table_column_types,
        reconcile_scalar_list_column, sync_join_tables, sync_versions_table, table_exists,
    },
};

/// Sync a global's schema: create or alter table, join tables, versions.
pub(super) fn sync_global_table(
    conn: &dyn DbConnection,
    slug: &str,
    def: &GlobalDefinition,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let table_name = global_table(slug);

    if table_exists(conn, &table_name)? {
        alter_global_table(conn, &table_name, def, locale_config)?;
    } else {
        create_global_table(conn, &table_name, def, locale_config)?;
    }

    sync_join_tables(conn, &table_name, &def.fields, locale_config)?;

    if def.has_versions() {
        sync_versions_table(conn, &table_name)?;
    }

    Ok(())
}

/// Build a column definition with optional default value.
fn build_col_def(
    col_name: &str,
    col_type: &str,
    companion_text: bool,
    field: &crate::core::FieldDefinition,
    db_kind: &str,
) -> String {
    let mut col = format!("{} {col_type}", quote_ident(col_name));

    if !companion_text {
        append_default_value_for(
            &mut col,
            field.default_value.as_ref(),
            &field.field_type,
            db_kind,
        );
    }

    col
}

/// Create a new global table with all field columns and a default row.
fn create_global_table(
    conn: &dyn DbConnection,
    table_name: &str,
    def: &GlobalDefinition,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let mut columns = vec!["id TEXT PRIMARY KEY".to_string()];

    for spec in &collect_column_specs(&def.fields, locale_config) {
        // Route through the shared `ddl_type` so a scalar `has_many` field (stored
        // as a JSON array in TEXT) isn't given a numeric column — the same rule
        // collection tables use. Skipping it here mistyped globals on Postgres.
        let col_type = spec.ddl_type(conn);

        if spec.is_localized {
            for locale in &locale_config.locales {
                let col_name = locale_column(&spec.col_name, locale)?;
                columns.push(build_col_def(
                    &col_name,
                    col_type,
                    spec.companion_text,
                    spec.field,
                    conn.kind(),
                ));
            }
        } else {
            columns.push(build_col_def(
                &spec.col_name,
                col_type,
                spec.companion_text,
                spec.field,
                conn.kind(),
            ));
        }
    }

    if def.has_drafts() {
        columns.push("_status TEXT NOT NULL DEFAULT 'published'".to_string());
    }

    columns.push("_ref_count INTEGER NOT NULL DEFAULT 0".to_string());
    columns.push(format!("created_at {}", conn.timestamp_column_default()));
    columns.push(format!("updated_at {}", conn.timestamp_column_default()));

    let sql = format!("CREATE TABLE \"{}\" ({})", table_name, columns.join(", "));

    info!("Creating global table: {}", table_name);

    conn.execute_ddl(&sql, &[])
        .with_context(|| format!("Failed to create table {table_name}"))?;

    conn.execute(
        &conn.build_insert_ignore(table_name, "id", "'default'"),
        &[],
    )?;

    Ok(())
}

/// Add missing columns to an existing global table.
fn alter_global_table(
    conn: &dyn DbConnection,
    table_name: &str,
    def: &GlobalDefinition,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let column_types = get_table_column_types(conn, table_name)?;
    let existing: HashSet<String> = column_types.keys().cloned().collect();

    add_field_columns(
        conn,
        table_name,
        def,
        locale_config,
        &existing,
        &column_types,
    )?;
    add_system_column(
        conn,
        table_name,
        "_status",
        "TEXT NOT NULL DEFAULT 'published'",
        def.has_drafts(),
        &existing,
    )?;
    add_system_column(
        conn,
        table_name,
        "_ref_count",
        "INTEGER NOT NULL DEFAULT 0",
        true,
        &existing,
    )?;

    Ok(())
}

/// Add missing field columns to a global table.
fn add_field_columns(
    conn: &dyn DbConnection,
    table_name: &str,
    def: &GlobalDefinition,
    locale_config: &LocaleConfig,
    existing: &HashSet<String>,
    column_types: &HashMap<String, String>,
) -> Result<()> {
    for spec in &collect_column_specs(&def.fields, locale_config) {
        if spec.is_localized {
            for locale in &locale_config.locales {
                let col_name = locale_column(&spec.col_name, locale)?;
                add_field_column_if_missing(
                    conn,
                    table_name,
                    &col_name,
                    spec,
                    existing,
                    column_types,
                )?;
            }
        } else {
            add_field_column_if_missing(
                conn,
                table_name,
                &spec.col_name,
                spec,
                existing,
                column_types,
            )?;
        }
    }

    Ok(())
}

/// Build a field column definition and add it if it doesn't already exist.
///
/// Routes the type through the shared `ColumnSpec::ddl_type` so a scalar
/// `has_many` field (a JSON array stored in TEXT) isn't given a numeric column
/// — the same rule collection tables use. Skipping it here mistyped globals on
/// Postgres.
fn add_field_column_if_missing(
    conn: &dyn DbConnection,
    table_name: &str,
    col_name: &str,
    spec: &ColumnSpec,
    existing: &HashSet<String>,
    column_types: &HashMap<String, String>,
) -> Result<()> {
    if existing.contains(col_name) {
        // Mirror the collection alter path: an existing scalar has-many column
        // mistyped as numeric on an older Postgres database is reconciled to
        // TEXT (else its JSON-array writes error after upgrade). No-op otherwise.
        if spec.field.is_has_many_scalar() {
            reconcile_scalar_list_column(conn, table_name, col_name, column_types)?;
        }

        return Ok(());
    }

    let col_def = build_col_def(
        col_name,
        spec.ddl_type(conn),
        spec.companion_text,
        spec.field,
        conn.kind(),
    );
    add_column_if_missing(conn, table_name, col_name, &col_def, existing)
}

/// Add a system column if condition is true and column doesn't exist.
fn add_system_column(
    conn: &dyn DbConnection,
    table_name: &str,
    col_name: &str,
    col_def: &str,
    condition: bool,
    existing: &HashSet<String>,
) -> Result<()> {
    if !condition {
        return Ok(());
    }

    let full_def = format!("{} {col_def}", quote_ident(col_name));
    add_column_if_missing(conn, table_name, col_name, &full_def, existing)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::collection::*;
    use crate::core::{FieldDefinition, FieldTab, FieldType};
    use crate::db::migrate::collection::test_helpers::*;
    use crate::db::migrate::helpers::get_table_columns;

    fn simple_global(slug: &str, fields: Vec<FieldDefinition>) -> GlobalDefinition {
        let mut def = GlobalDefinition::new(slug);
        def.fields = fields;
        def
    }

    /// Sync a global and return its column names.
    fn sync_and_columns(def: &GlobalDefinition, locale: &LocaleConfig) -> HashSet<String> {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        sync_global_table(&conn, &def.slug, def, locale).unwrap();
        get_table_columns(&conn, &global_table(&def.slug)).unwrap()
    }

    /// Sync two defs sequentially (create then alter) and return columns.
    fn sync_alter_and_columns(
        def1: &GlobalDefinition,
        def2: &GlobalDefinition,
        locale: &LocaleConfig,
    ) -> HashSet<String> {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        sync_global_table(&conn, &def1.slug, def1, locale).unwrap();
        sync_global_table(&conn, &def1.slug, def2, locale).unwrap();
        get_table_columns(&conn, &global_table(&def1.slug)).unwrap()
    }

    // ── create ──────────────────────────────────────────────────────────

    #[test]
    fn global_table_created_with_default_row() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_global("settings", vec![text_field("site_name")]);
        sync_global_table(&conn, "settings", &def, &no_locale()).unwrap();

        assert!(table_exists(&conn, "_global_settings").unwrap());
        let row = conn
            .query_one("SELECT COUNT(*) AS cnt FROM _global_settings", &[])
            .unwrap()
            .unwrap();
        assert_eq!(row.get_i64("cnt").unwrap(), 1);
    }

    /// Regression: a top-level scalar `has_many` field stores a JSON array and
    /// must be a TEXT column (a numeric column rejects the array on Postgres).
    /// Globals previously bypassed `ColumnSpec::ddl_type` and gave it the numeric
    /// type. Declared type differs on `SQLite` too (TEXT vs REAL), so this catches
    /// it on either backend.
    #[test]
    fn global_has_many_scalar_column_is_text_not_numeric() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let field = FieldDefinition::builder("scores", FieldType::Number)
            .has_many(true)
            .build();
        let def = simple_global("settings", vec![field]);
        sync_global_table(&conn, "settings", &def, &no_locale()).unwrap();

        let types = conn.get_table_column_types("_global_settings").unwrap();
        assert_eq!(
            types.get("scores").map(String::as_str),
            Some("TEXT"),
            "has_many scalar global column must be TEXT, got {:?}",
            types.get("scores")
        );
    }

    // ── alter ───────────────────────────────────────────────────────────

    #[test]
    fn global_table_alter_adds_new_column() {
        let def1 = simple_global("settings", vec![text_field("site_name")]);
        let def2 = simple_global(
            "settings",
            vec![text_field("site_name"), text_field("site_url")],
        );
        let cols = sync_alter_and_columns(&def1, &def2, &no_locale());
        assert!(cols.contains("site_url"));
    }

    // ── localized ───────────────────────────────────────────────────────

    #[test]
    fn global_table_localized_fields() {
        let def = simple_global("settings", vec![localized_field("site_name")]);
        let cols = sync_and_columns(&def, &locale_en_de());
        assert!(cols.contains("site_name__en"));
        assert!(cols.contains("site_name__de"));
        assert!(!cols.contains("site_name"));
    }

    #[test]
    fn global_table_alter_adds_localized_columns() {
        let def1 = simple_global("settings", vec![text_field("name")]);
        let def2 = simple_global(
            "settings",
            vec![text_field("name"), localized_field("description")],
        );
        let cols = sync_alter_and_columns(&def1, &def2, &locale_en_de());
        assert!(cols.contains("description__en"));
        assert!(cols.contains("description__de"));
    }

    // ── group fields ────────────────────────────────────────────────────

    #[test]
    fn global_table_group_fields_create() {
        let def = simple_global(
            "settings",
            vec![
                FieldDefinition::builder("seo", FieldType::Group)
                    .fields(vec![text_field("title"), text_field("description")])
                    .build(),
            ],
        );
        let cols = sync_and_columns(&def, &no_locale());
        assert!(cols.contains("seo__title"));
        assert!(cols.contains("seo__description"));
    }

    #[test]
    fn global_table_group_fields_alter() {
        let def1 = simple_global("settings", vec![text_field("name")]);
        let def2 = simple_global(
            "settings",
            vec![
                text_field("name"),
                FieldDefinition::builder("seo", FieldType::Group)
                    .fields(vec![text_field("title")])
                    .build(),
            ],
        );
        let cols = sync_alter_and_columns(&def1, &def2, &no_locale());
        assert!(cols.contains("seo__title"));
    }

    // ── localized group fields ──────────────────────────────────────────

    #[test]
    fn global_table_localized_group_create() {
        let def = simple_global(
            "settings",
            vec![
                FieldDefinition::builder("seo", FieldType::Group)
                    .localized(true)
                    .fields(vec![text_field("title")])
                    .build(),
            ],
        );
        let cols = sync_and_columns(&def, &locale_en_de());
        assert!(cols.contains("seo__title__en"));
        assert!(cols.contains("seo__title__de"));
    }

    #[test]
    fn global_table_localized_group_alter() {
        let def1 = simple_global("settings", vec![text_field("name")]);
        let def2 = simple_global(
            "settings",
            vec![
                text_field("name"),
                FieldDefinition::builder("seo", FieldType::Group)
                    .localized(true)
                    .fields(vec![text_field("title")])
                    .build(),
            ],
        );
        let cols = sync_alter_and_columns(&def1, &def2, &locale_en_de());
        assert!(cols.contains("seo__title__en"));
        assert!(cols.contains("seo__title__de"));
    }

    // ── versioned ───────────────────────────────────────────────────────

    #[test]
    fn versioned_global_creates_versions_table() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let mut def = simple_global("settings", vec![text_field("name")]);
        def.versions = Some(VersionsConfig::new(true, 5));
        sync_global_table(&conn, "settings", &def, &no_locale()).unwrap();
        assert!(table_exists(&conn, "_versions__global_settings").unwrap());
        let cols = get_table_columns(&conn, "_global_settings").unwrap();
        assert!(cols.contains("_status"));
    }

    #[test]
    fn global_table_alter_adds_status_for_drafts() {
        let def1 = simple_global("settings", vec![text_field("name")]);
        let mut def2 = simple_global("settings", vec![text_field("name")]);
        def2.versions = Some(VersionsConfig::new(true, 5));
        let cols = sync_alter_and_columns(&def1, &def2, &no_locale());
        assert!(cols.contains("_status"));
    }

    // ── join tables ─────────────────────────────────────────────────────

    #[test]
    fn global_table_creates_join_tables() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_global(
            "settings",
            vec![
                FieldDefinition::builder("items", FieldType::Array)
                    .fields(vec![text_field("label")])
                    .build(),
            ],
        );
        sync_global_table(&conn, "settings", &def, &no_locale()).unwrap();
        assert!(table_exists(&conn, "_global_settings_items").unwrap());
    }

    // ── layout wrappers ─────────────────────────────────────────────────

    #[test]
    fn global_table_collapsible_promotes_flat() {
        let def = simple_global(
            "settings",
            vec![
                FieldDefinition::builder("extra", FieldType::Collapsible)
                    .fields(vec![text_field("notes"), text_field("footer")])
                    .build(),
            ],
        );
        let cols = sync_and_columns(&def, &no_locale());
        assert!(cols.contains("notes"));
        assert!(cols.contains("footer"));
        assert!(!cols.contains("extra"));
    }

    #[test]
    fn global_table_tabs_promotes_flat() {
        let def = simple_global(
            "settings",
            vec![
                FieldDefinition::builder("layout", FieldType::Tabs)
                    .tabs(vec![
                        FieldTab::new("General", vec![text_field("site_name")]),
                        FieldTab::new("Footer", vec![text_field("copyright")]),
                    ])
                    .build(),
            ],
        );
        let cols = sync_and_columns(&def, &no_locale());
        assert!(cols.contains("site_name"));
        assert!(cols.contains("copyright"));
        assert!(!cols.contains("layout"));
    }

    #[test]
    fn global_table_tabs_with_group_creates_prefixed_columns() {
        let def = simple_global(
            "settings",
            vec![
                FieldDefinition::builder("layout", FieldType::Tabs)
                    .tabs(vec![FieldTab::new(
                        "Social",
                        vec![
                            FieldDefinition::builder("social", FieldType::Group)
                                .fields(vec![text_field("github"), text_field("twitter")])
                                .build(),
                        ],
                    )])
                    .build(),
            ],
        );
        let cols = sync_and_columns(&def, &no_locale());
        assert!(cols.contains("social__github"));
        assert!(cols.contains("social__twitter"));
        assert!(!cols.contains("social"));
    }

    #[test]
    fn global_table_collapsible_with_group_creates_prefixed_columns() {
        let def = simple_global(
            "settings",
            vec![
                FieldDefinition::builder("extra", FieldType::Collapsible)
                    .fields(vec![
                        FieldDefinition::builder("seo", FieldType::Group)
                            .fields(vec![text_field("title"), text_field("desc")])
                            .build(),
                    ])
                    .build(),
            ],
        );
        let cols = sync_and_columns(&def, &no_locale());
        assert!(cols.contains("seo__title"));
        assert!(cols.contains("seo__desc"));
    }

    #[test]
    fn global_table_alter_adds_tabs_with_group() {
        let def1 = simple_global("settings", vec![text_field("name")]);
        let def2 = simple_global(
            "settings",
            vec![
                text_field("name"),
                FieldDefinition::builder("layout", FieldType::Tabs)
                    .tabs(vec![FieldTab::new(
                        "Social",
                        vec![
                            FieldDefinition::builder("social", FieldType::Group)
                                .fields(vec![text_field("github")])
                                .build(),
                        ],
                    )])
                    .build(),
            ],
        );
        let cols = sync_alter_and_columns(&def1, &def2, &no_locale());
        assert!(cols.contains("social__github"));
    }

    // ── deeply nested ───────────────────────────────────────────────────

    #[test]
    fn global_deeply_nested_layout() {
        let def = simple_global(
            "settings",
            vec![
                FieldDefinition::builder("layout", FieldType::Tabs)
                    .tabs(vec![FieldTab::new(
                        "Advanced",
                        vec![
                            FieldDefinition::builder("advanced", FieldType::Collapsible)
                                .fields(vec![
                                    FieldDefinition::builder("og", FieldType::Group)
                                        .fields(vec![text_field("image")])
                                        .build(),
                                    text_field("canonical"),
                                ])
                                .build(),
                        ],
                    )])
                    .build(),
            ],
        );
        let cols = sync_and_columns(&def, &no_locale());
        assert!(cols.contains("og__image"));
        assert!(cols.contains("canonical"));
    }

    #[test]
    fn global_group_containing_row() {
        let def = simple_global(
            "settings",
            vec![
                FieldDefinition::builder("branding", FieldType::Group)
                    .fields(vec![
                        FieldDefinition::builder("row1", FieldType::Row)
                            .fields(vec![text_field("logo"), text_field("favicon")])
                            .build(),
                    ])
                    .build(),
            ],
        );
        let cols = sync_and_columns(&def, &no_locale());
        assert!(cols.contains("branding__logo"));
        assert!(cols.contains("branding__favicon"));
    }

    #[test]
    fn global_group_containing_tabs() {
        let def = simple_global(
            "settings",
            vec![
                FieldDefinition::builder("config", FieldType::Group)
                    .fields(vec![
                        FieldDefinition::builder("layout", FieldType::Tabs)
                            .tabs(vec![
                                FieldTab::new("General", vec![text_field("site_name")]),
                                FieldTab::new("Social", vec![text_field("twitter")]),
                            ])
                            .build(),
                    ])
                    .build(),
            ],
        );
        let cols = sync_and_columns(&def, &no_locale());
        assert!(cols.contains("config__site_name"));
        assert!(cols.contains("config__twitter"));
    }

    #[test]
    fn global_group_tabs_group_three_levels() {
        let def = simple_global(
            "settings",
            vec![
                FieldDefinition::builder("a", FieldType::Group)
                    .fields(vec![
                        FieldDefinition::builder("t", FieldType::Tabs)
                            .tabs(vec![FieldTab::new(
                                "Tab",
                                vec![
                                    FieldDefinition::builder("b", FieldType::Group)
                                        .fields(vec![text_field("leaf")])
                                        .build(),
                                ],
                            )])
                            .build(),
                    ])
                    .build(),
            ],
        );
        let cols = sync_and_columns(&def, &no_locale());
        assert!(cols.contains("a__b__leaf"));
    }

    // ── companion columns (timezone _tz) ────────────────────────────────

    #[test]
    fn global_table_date_timezone_creates_companion_column() {
        let def = simple_global(
            "settings",
            vec![
                FieldDefinition::builder("event_at", FieldType::Date)
                    .timezone(true)
                    .build(),
            ],
        );
        let cols = sync_and_columns(&def, &no_locale());
        assert!(cols.contains("event_at"));
        assert!(cols.contains("event_at_tz"));
    }

    #[test]
    fn global_table_alter_adds_date_timezone_companion_column() {
        let def1 = simple_global("settings", vec![text_field("name")]);
        let def2 = simple_global(
            "settings",
            vec![
                text_field("name"),
                FieldDefinition::builder("event_at", FieldType::Date)
                    .timezone(true)
                    .build(),
            ],
        );
        let cols = sync_alter_and_columns(&def1, &def2, &no_locale());
        assert!(cols.contains("event_at_tz"));
    }

    #[test]
    fn global_table_localized_date_timezone_creates_companion_columns() {
        let def = simple_global(
            "settings",
            vec![
                FieldDefinition::builder("event_at", FieldType::Date)
                    .timezone(true)
                    .localized(true)
                    .build(),
            ],
        );
        let cols = sync_and_columns(&def, &locale_en_de());
        assert!(cols.contains("event_at__en"));
        assert!(cols.contains("event_at__de"));
        assert!(cols.contains("event_at_tz__en"));
        assert!(cols.contains("event_at_tz__de"));
    }

    // ── default values ──────────────────────────────────────────────────

    #[test]
    fn global_table_creates_with_default_values() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_global(
            "settings",
            vec![
                FieldDefinition::builder("site_name", FieldType::Text)
                    .default_value(serde_json::Value::String("My Site".to_string()))
                    .build(),
                FieldDefinition::builder("enabled", FieldType::Checkbox).build(),
            ],
        );
        sync_global_table(&conn, "settings", &def, &no_locale()).unwrap();

        // SQLite inserts NULL for the default row (INSERT OR IGNORE with just id),
        // but the column DEFAULT is correctly set. Verify by inserting a new row.
        conn.execute_batch("INSERT INTO _global_settings (id) VALUES ('test_defaults')")
            .unwrap();
        let row = conn
            .query_one(
                "SELECT site_name, enabled FROM _global_settings WHERE id = 'test_defaults'",
                &[],
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            row.get_opt_string("site_name").unwrap(),
            Some("My Site".to_string()),
            "Text field should have DEFAULT applied"
        );
        assert_eq!(
            row.get_i64("enabled").unwrap(),
            0,
            "Checkbox should have DEFAULT 0"
        );
    }

    #[test]
    fn global_table_alter_adds_column_with_default() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def1 = simple_global("settings", vec![text_field("name")]);
        sync_global_table(&conn, "settings", &def1, &no_locale()).unwrap();

        let def2 = simple_global(
            "settings",
            vec![
                text_field("name"),
                FieldDefinition::builder("mode", FieldType::Text)
                    .default_value(serde_json::Value::String("dark".to_string()))
                    .build(),
            ],
        );
        sync_global_table(&conn, "settings", &def2, &no_locale()).unwrap();

        // Verify default by inserting a row that relies on DEFAULT
        conn.execute_batch("INSERT INTO _global_settings (id) VALUES ('test_alter_default')")
            .unwrap();
        let row = conn
            .query_one(
                "SELECT mode FROM _global_settings WHERE id = 'test_alter_default'",
                &[],
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            row.get_opt_string("mode").unwrap(),
            Some("dark".to_string()),
            "ALTER-added column should have DEFAULT applied"
        );
    }

    /// Regression: an existing scalar `has_many` column on a GLOBAL table that
    /// drifted to a numeric physical type on an older Postgres database is
    /// reconciled back to TEXT on the next migration — else its JSON-array
    /// writes error after upgrade. Mirrors the collection alter path; globals
    /// previously lacked the reconcile limb. Skips when `TEST_DATABASE_URL` is
    /// unset. Postgres-only (`SQLite` never drifts the affinity).
    #[cfg(feature = "postgres")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pg_global_has_many_scalar_column_reconciled_to_text() {
        let Some(pool) = crate::db::pg_test::pg_test_pool() else {
            eprintln!("skipping: TEST_DATABASE_URL not set");
            return;
        };

        let conn = pool.get().expect("get PG connection");
        let slug = crate::db::pg_test::unique_slug("gcfg");
        let table = global_table(&slug);

        let def = simple_global(
            &slug,
            vec![
                FieldDefinition::builder("scores", FieldType::Number)
                    .has_many(true)
                    .build(),
            ],
        );

        // First migration creates the table with the correct TEXT column.
        sync_global_table(&conn, &slug, &def, &no_locale()).unwrap();

        // Simulate an OLD database whose column drifted to numeric.
        conn.execute_ddl(
            &format!(
                "ALTER TABLE {} ALTER COLUMN scores TYPE DOUBLE PRECISION USING NULL",
                quote_ident(&table)
            ),
            &[],
        )
        .unwrap();
        let scores_type = |table: &str| -> String {
            conn.get_table_column_types(table)
                .unwrap()
                .get("scores")
                .cloned()
                .unwrap_or_default()
        };
        assert!(
            !scores_type(&table).eq_ignore_ascii_case("text"),
            "precondition: column must be numeric after the simulated drift"
        );

        // Re-running the migration must reconcile it back to TEXT.
        sync_global_table(&conn, &slug, &def, &no_locale()).unwrap();
        assert!(
            scores_type(&table).eq_ignore_ascii_case("text"),
            "global reconcile must flip the drifted has-many-scalar column back to TEXT, got {:?}",
            scores_type(&table)
        );

        conn.execute_ddl(&format!("DROP TABLE {}", quote_ident(&table)), &[])
            .unwrap();
    }
}
