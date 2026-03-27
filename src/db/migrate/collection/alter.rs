//! ALTER TABLE operations for existing collection tables.

use anyhow::{Context as _, Result};
use std::collections::{HashMap, HashSet};

use crate::{
    config::LocaleConfig,
    core::CollectionDefinition,
    db::{
        DbConnection,
        migrate::helpers::{
            collect_column_specs, get_table_column_types, get_table_columns, sanitize_locale,
        },
    },
};

use super::create::append_default_value;

/// Shared context for ALTER TABLE operations.
struct AlterCtx<'a> {
    conn: &'a dyn DbConnection,
    slug: &'a str,
    def: &'a CollectionDefinition,
    existing: &'a HashSet<String>,
    /// Column name -> DB type (from PRAGMA table_info) for type mismatch detection.
    column_types: &'a HashMap<String, String>,
}

impl<'a> AlterCtx<'a> {
    fn builder(conn: &'a dyn DbConnection, slug: &'a str) -> AlterCtxBuilder<'a> {
        AlterCtxBuilder {
            conn,
            slug,
            def: None,
            existing: None,
            column_types: None,
        }
    }
}

/// Builder for [`AlterCtx`].
struct AlterCtxBuilder<'a> {
    conn: &'a dyn DbConnection,
    slug: &'a str,
    def: Option<&'a CollectionDefinition>,
    existing: Option<&'a HashSet<String>>,
    column_types: Option<&'a HashMap<String, String>>,
}

impl<'a> AlterCtxBuilder<'a> {
    fn def(mut self, v: &'a CollectionDefinition) -> Self {
        self.def = Some(v);
        self
    }

    fn existing(mut self, v: &'a HashSet<String>) -> Self {
        self.existing = Some(v);
        self
    }

    fn column_types(mut self, v: &'a HashMap<String, String>) -> Self {
        self.column_types = Some(v);
        self
    }

    fn build(self) -> AlterCtx<'a> {
        AlterCtx {
            conn: self.conn,
            slug: self.slug,
            def: self.def.expect("AlterCtx requires def"),
            existing: self.existing.expect("AlterCtx requires existing"),
            column_types: self.column_types.expect("AlterCtx requires column_types"),
        }
    }
}

/// Warn if an existing column's DB type differs from the expected type.
fn warn_type_mismatch(ctx: &AlterCtx, col_name: &str, expected_type: &str) {
    if let Some(db_type) = ctx.column_types.get(col_name)
        && !db_type.eq_ignore_ascii_case(expected_type)
    {
        tracing::warn!(
            "Column '{}' in table '{}' has type '{}' but definition expects '{}' \
             (not auto-migrated — manual migration required)",
            col_name,
            ctx.slug,
            db_type,
            expected_type
        );
    }
}

/// Add missing user-defined field columns (including localized variants).
fn add_field_columns(ctx: &AlterCtx, locale_config: &LocaleConfig) -> Result<()> {
    for spec in &collect_column_specs(&ctx.def.fields, locale_config) {
        let expected_type = if spec.companion_text {
            "TEXT"
        } else {
            ctx.conn.column_type_for(&spec.field.field_type)
        };

        if spec.is_localized {
            for locale in &locale_config.locales {
                let col_name = format!("{}__{}", spec.col_name, sanitize_locale(locale)?);

                if ctx.existing.contains(&col_name) {
                    warn_type_mismatch(ctx, &col_name, expected_type);
                } else {
                    let mut col_def = expected_type.to_string();

                    if !spec.companion_text {
                        append_default_value(
                            &mut col_def,
                            &spec.field.default_value,
                            &spec.field.field_type,
                        );
                    }

                    let sql = format!(
                        "ALTER TABLE {} ADD COLUMN {} {}",
                        ctx.slug, col_name, col_def
                    );
                    tracing::info!("Adding column to {}: {}", ctx.slug, col_name);
                    ctx.conn.execute(&sql, &[]).with_context(|| {
                        format!("Failed to add column {} to {}", col_name, ctx.slug)
                    })?;
                }
            }
        } else if ctx.existing.contains(&spec.col_name) {
            warn_type_mismatch(ctx, &spec.col_name, expected_type);
        } else {
            let mut col_def = expected_type.to_string();

            if !spec.companion_text {
                append_default_value(
                    &mut col_def,
                    &spec.field.default_value,
                    &spec.field.field_type,
                );
            }

            let sql = format!(
                "ALTER TABLE {} ADD COLUMN {} {}",
                ctx.slug, spec.col_name, col_def
            );
            tracing::info!("Adding column to {}: {}", ctx.slug, spec.col_name);
            ctx.conn.execute(&sql, &[]).with_context(|| {
                format!("Failed to add column {} to {}", spec.col_name, ctx.slug)
            })?;
        }
    }

    Ok(())
}

/// Add system columns (_status, auth, timestamps) as needed.
fn add_system_columns(ctx: &AlterCtx) -> Result<()> {
    // Versioned collections with drafts: ensure _status column exists
    if ctx.def.has_drafts() && !ctx.existing.contains("_status") {
        let sql = format!(
            "ALTER TABLE {} ADD COLUMN _status TEXT NOT NULL DEFAULT 'published'",
            ctx.slug
        );
        tracing::info!("Adding _status column to {}", ctx.slug);
        ctx.conn
            .execute(&sql, &[])
            .with_context(|| format!("Failed to add _status to {}", ctx.slug))?;
    }

    // Auth collections: ensure system columns exist
    if ctx.def.is_auth_collection() {
        for col in [
            "_password_hash TEXT",
            "_reset_token TEXT",
            "_reset_token_exp INTEGER",
            "_locked INTEGER DEFAULT 0",
            "_settings TEXT",
            "_session_version INTEGER DEFAULT 0",
        ] {
            let col_name = col
                .split_whitespace()
                .next()
                .expect("static column definition");

            if !ctx.existing.contains(col_name) {
                let sql = format!("ALTER TABLE {} ADD COLUMN {}", ctx.slug, col);
                tracing::info!("Adding {} column to {}", col_name, ctx.slug);
                ctx.conn
                    .execute(&sql, &[])
                    .with_context(|| format!("Failed to add {} to {}", col_name, ctx.slug))?;
            }
        }
        if ctx.def.auth.as_ref().is_some_and(|a| a.verify_email) {
            for col in [
                "_verified INTEGER DEFAULT 0",
                "_verification_token TEXT",
                "_verification_token_exp INTEGER",
            ] {
                let col_name = col
                    .split_whitespace()
                    .next()
                    .expect("static column definition");

                if !ctx.existing.contains(col_name) {
                    let sql = format!("ALTER TABLE {} ADD COLUMN {}", ctx.slug, col);
                    tracing::info!("Adding {} column to {}", col_name, ctx.slug);
                    ctx.conn
                        .execute(&sql, &[])
                        .with_context(|| format!("Failed to add {} to {}", col_name, ctx.slug))?;
                }
            }
        }
    }

    // Timestamps: ensure created_at/updated_at exist
    // Note: SQLite ALTER TABLE cannot use non-constant defaults like datetime('now'),
    // so we add with no default (NULL for existing rows) — new inserts set these explicitly.
    if ctx.def.timestamps {
        for col_name in ["created_at", "updated_at"] {
            if !ctx.existing.contains(col_name) {
                let ts_type = ctx.conn.timestamp_column_type();
                let sql = format!(
                    "ALTER TABLE {} ADD COLUMN {} {}",
                    ctx.slug, col_name, ts_type
                );
                tracing::info!("Adding {} column to {}", col_name, ctx.slug);
                ctx.conn
                    .execute(&sql, &[])
                    .with_context(|| format!("Failed to add {} to {}", col_name, ctx.slug))?;
            }
        }
    }

    Ok(())
}

/// Build the set of expected column names from field definitions (for orphan detection).
/// Delegates to `collect_column_specs` so arbitrary nesting of Group, Row, Collapsible,
/// and Tabs is handled identically to schema creation/alteration.
fn collect_expected_column_names(
    def: &CollectionDefinition,
    locale_config: &LocaleConfig,
) -> HashSet<String> {
    collect_column_specs(&def.fields, locale_config)
        .into_iter()
        .map(|spec| spec.col_name)
        .collect()
}

/// System columns that are always valid (not flagged as orphans).
const SYSTEM_COLUMNS: &[&str] = &[
    "id",
    "created_at",
    "updated_at",
    "_password_hash",
    "_reset_token",
    "_reset_token_exp",
    "_verified",
    "_verification_token",
    "_verification_token_exp",
    "_locked",
    "_status",
    "_settings",
    "_session_version",
];

pub(super) fn alter_collection_table(
    conn: &dyn DbConnection,
    slug: &str,
    def: &CollectionDefinition,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let existing = get_table_columns(conn, slug)?;
    let column_types = get_table_column_types(conn, slug)?;
    let ctx = AlterCtx::builder(conn, slug)
        .def(def)
        .existing(&existing)
        .column_types(&column_types)
        .build();

    add_field_columns(&ctx, locale_config)?;
    add_system_columns(&ctx)?;

    // Warn about removed columns (SQLite can't DROP COLUMN easily)
    let expected = collect_expected_column_names(def, locale_config);
    let system: HashSet<&str> = SYSTEM_COLUMNS.iter().copied().collect();

    for col in &existing {
        if !expected.contains(col) && !system.contains(col.as_str()) {
            tracing::warn!(
                "Column '{}' exists in table '{}' but not in Lua definition (not removed)",
                col,
                slug
            );
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::create::create_collection_table;
    use super::super::test_helpers::*;
    use super::*;
    use crate::core::collection::*;
    use crate::core::field::{FieldDefinition, FieldTab, FieldType};
    use crate::db::migrate::helpers::get_table_columns;

    #[test]
    fn alter_adds_new_column() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def1 = simple_collection("posts", vec![text_field("title")]);
        create_collection_table(&conn, "posts", &def1, &no_locale()).unwrap();

        let def2 = simple_collection("posts", vec![text_field("title"), text_field("summary")]);
        alter_collection_table(&conn, "posts", &def2, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("summary"), "new column should be added");
    }

    #[test]
    fn alter_adds_auth_system_columns() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def1 = simple_collection("users", vec![text_field("email")]);
        create_collection_table(&conn, "users", &def1, &no_locale()).unwrap();

        // Now make it an auth collection with verify_email
        let mut def2 = simple_collection("users", vec![text_field("email")]);
        def2.auth = Some(Auth {
            enabled: true,
            verify_email: true,
            ..Default::default()
        });
        alter_collection_table(&conn, "users", &def2, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "users").unwrap();
        assert!(cols.contains("_password_hash"));
        assert!(cols.contains("_reset_token"));
        assert!(cols.contains("_reset_token_exp"));
        assert!(cols.contains("_locked"));
        assert!(cols.contains("_settings"));
        assert!(cols.contains("_session_version"));
        assert!(cols.contains("_verified"));
        assert!(cols.contains("_verification_token"));
    }

    #[test]
    fn alter_adds_status_for_drafts() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def1 = simple_collection("posts", vec![text_field("title")]);
        create_collection_table(&conn, "posts", &def1, &no_locale()).unwrap();

        // Enable drafts on existing collection
        let mut def2 = simple_collection("posts", vec![text_field("title")]);
        def2.versions = Some(VersionsConfig::new(true, 5));
        alter_collection_table(&conn, "posts", &def2, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("_status"));
    }

    #[test]
    fn alter_adds_timestamps() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        // Create a table without timestamps
        conn.execute("CREATE TABLE posts (id TEXT PRIMARY KEY, title TEXT)", &[])
            .unwrap();

        let def = simple_collection("posts", vec![text_field("title")]);
        alter_collection_table(&conn, "posts", &def, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("created_at"));
        assert!(cols.contains("updated_at"));
    }

    #[test]
    fn alter_collection_with_localized_fields() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![localized_field("title")]);
        create_collection_table(&conn, "posts", &def, &locale_en_de()).unwrap();

        // Add a new localized field via alter
        let def2 = simple_collection(
            "posts",
            vec![localized_field("title"), localized_field("body")],
        );
        alter_collection_table(&conn, "posts", &def2, &locale_en_de()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("body__en"));
        assert!(cols.contains("body__de"));
    }

    #[test]
    fn alter_adds_group_fields() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def1 = simple_collection("posts", vec![text_field("title")]);
        create_collection_table(&conn, "posts", &def1, &no_locale()).unwrap();

        let def2 = simple_collection(
            "posts",
            vec![
                text_field("title"),
                FieldDefinition::builder("seo", FieldType::Group)
                    .fields(vec![text_field("meta_title"), text_field("meta_desc")])
                    .build(),
            ],
        );
        alter_collection_table(&conn, "posts", &def2, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("seo__meta_title"));
        assert!(cols.contains("seo__meta_desc"));
    }

    #[test]
    fn alter_adds_localized_group_fields() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def1 = simple_collection("posts", vec![text_field("title")]);
        create_collection_table(&conn, "posts", &def1, &locale_en_de()).unwrap();

        let def2 = simple_collection(
            "posts",
            vec![
                text_field("title"),
                FieldDefinition::builder("seo", FieldType::Group)
                    .localized(true)
                    .fields(vec![text_field("meta_title")])
                    .build(),
            ],
        );
        alter_collection_table(&conn, "posts", &def2, &locale_en_de()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("seo__meta_title__en"));
        assert!(cols.contains("seo__meta_title__de"));
    }

    #[test]
    fn alter_adds_row_sub_fields() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def1 = simple_collection("posts", vec![text_field("title")]);
        create_collection_table(&conn, "posts", &def1, &no_locale()).unwrap();

        let def2 = simple_collection(
            "posts",
            vec![
                text_field("title"),
                FieldDefinition::builder("names", FieldType::Row)
                    .fields(vec![text_field("first"), text_field("last")])
                    .build(),
            ],
        );
        alter_collection_table(&conn, "posts", &def2, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("first"));
        assert!(cols.contains("last"));
    }

    #[test]
    fn alter_adds_collapsible_sub_fields() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def1 = simple_collection("posts", vec![text_field("title")]);
        create_collection_table(&conn, "posts", &def1, &no_locale()).unwrap();

        let def2 = simple_collection(
            "posts",
            vec![
                text_field("title"),
                FieldDefinition::builder("extra", FieldType::Collapsible)
                    .fields(vec![text_field("notes")])
                    .build(),
            ],
        );
        alter_collection_table(&conn, "posts", &def2, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("notes"));
    }

    #[test]
    fn alter_adds_tabs_sub_fields() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def1 = simple_collection("posts", vec![text_field("title")]);
        create_collection_table(&conn, "posts", &def1, &no_locale()).unwrap();

        let def2 = simple_collection(
            "posts",
            vec![
                text_field("title"),
                FieldDefinition::builder("tabs", FieldType::Tabs)
                    .tabs(vec![FieldTab::new("T1", vec![text_field("body")])])
                    .build(),
            ],
        );
        alter_collection_table(&conn, "posts", &def2, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("body"));
    }

    #[test]
    fn alter_adds_tabs_with_group_sub_fields() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def1 = simple_collection("posts", vec![text_field("title")]);
        create_collection_table(&conn, "posts", &def1, &no_locale()).unwrap();

        let def2 = simple_collection(
            "posts",
            vec![
                text_field("title"),
                FieldDefinition::builder("tabs", FieldType::Tabs)
                    .tabs(vec![FieldTab::new(
                        "SEO",
                        vec![
                            FieldDefinition::builder("seo", FieldType::Group)
                                .fields(vec![text_field("og_title"), text_field("og_desc")])
                                .build(),
                        ],
                    )])
                    .build(),
            ],
        );
        alter_collection_table(&conn, "posts", &def2, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(
            cols.contains("seo__og_title"),
            "ALTER should add Group columns inside Tabs"
        );
        assert!(
            cols.contains("seo__og_desc"),
            "ALTER should add Group columns inside Tabs"
        );
    }

    #[test]
    fn orphan_detection_handles_deeply_nested_groups() {
        let fields = vec![FieldDefinition {
            name: "outer".into(),
            field_type: FieldType::Group,
            fields: vec![FieldDefinition {
                name: "inner".into(),
                field_type: FieldType::Group,
                fields: vec![text_field("deep")],
                ..Default::default()
            }],
            ..Default::default()
        }];
        let def = simple_collection("posts", fields);
        let names = collect_expected_column_names(&def, &no_locale());
        assert!(
            names.contains("outer__inner__deep"),
            "deeply nested Group sub-field should be tracked: {names:?}"
        );
    }

    #[test]
    fn orphan_detection_handles_group_inside_collapsible() {
        let fields = vec![FieldDefinition {
            name: "wrapper".into(),
            field_type: FieldType::Collapsible,
            fields: vec![FieldDefinition {
                name: "seo".into(),
                field_type: FieldType::Group,
                fields: vec![text_field("title"), text_field("description")],
                ..Default::default()
            }],
            ..Default::default()
        }];
        let def = simple_collection("posts", fields);
        let names = collect_expected_column_names(&def, &no_locale());
        assert!(
            names.contains("seo__title"),
            "Group inside Collapsible should be tracked: {names:?}"
        );
        assert!(
            names.contains("seo__description"),
            "Group inside Collapsible should be tracked: {names:?}"
        );
    }

    #[test]
    fn alter_adds_timezone_companion_column() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def1 = simple_collection("events", vec![text_field("title")]);
        create_collection_table(&conn, "events", &def1, &no_locale()).unwrap();

        let def2 = simple_collection(
            "events",
            vec![
                text_field("title"),
                FieldDefinition::builder("starts_at", FieldType::Date)
                    .timezone(true)
                    .build(),
            ],
        );
        alter_collection_table(&conn, "events", &def2, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "events").unwrap();
        assert!(cols.contains("starts_at"), "should add main date column");
        assert!(
            cols.contains("starts_at_tz"),
            "should add companion timezone column"
        );
    }
}
