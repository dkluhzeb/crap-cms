//! ALTER TABLE operations for existing collection tables.

use anyhow::Result;
use std::collections::{HashMap, HashSet};
use tracing::warn;

use crate::{
    config::LocaleConfig,
    core::{CollectionDefinition, collection::MfaMode},
    db::{
        DbConnection,
        migrate::{
            helpers::{
                ColumnSpec, add_column_if_missing, check_type_mismatch, collect_column_specs,
                get_table_column_types, reconcile_scalar_list_column, warn_orphan_columns,
            },
            locale_change::{LocaleShape, column_plans},
        },
        query::helpers::locale_column,
    },
};

use super::rebuild::PendingConstraints;
use super::soft_delete::check_no_trashed_rows;
use super::system_columns::{
    AUTH_COLUMNS, DRAFT_STATUS_COLUMN, MFA_COLUMNS, REF_COUNT_COLUMN, REVISION_COLUMN,
    TOTP_COLUMNS, VERIFY_EMAIL_COLUMNS,
};
use crate::core::collection::Auth;

/// Shared context for ALTER TABLE operations. All fields are required and
/// the struct is constructed in one place (`alter_collection_table`); plain
/// struct-literal construction is preferred over a panic-on-missing builder.
struct AlterCtx<'a> {
    conn: &'a dyn DbConnection,
    slug: &'a str,
    def: &'a CollectionDefinition,
    existing: &'a HashSet<String>,
    /// Column name -> DB type (from PRAGMA `table_info`) for type mismatch detection.
    column_types: &'a HashMap<String, String>,
}

/// Add a single field column if it doesn't exist, with optional default value.
/// Returns whether the column was created by this call.
fn add_field_column(
    ctx: &AlterCtx,
    col_name: &str,
    expected_type: &str,
    spec: &ColumnSpec,
) -> Result<bool> {
    if ctx.existing.contains(col_name) {
        if spec.field.is_has_many_scalar() {
            reconcile_scalar_list_column(ctx.conn, ctx.slug, col_name, ctx.column_types)?;
        } else {
            check_type_mismatch(ctx.slug, ctx.column_types, col_name, expected_type)?;
        }

        return Ok(false);
    }

    let col_def = spec.column_def(ctx.conn, col_name);
    add_column_if_missing(ctx.conn, ctx.slug, col_name, &col_def, ctx.existing)?;

    Ok(true)
}

/// Add missing user-defined field columns (including localized variants).
///
/// A field whose `localized` flag changed reads its values from another column
/// from then on — the plan carries them across so the content stays reachable
/// instead of being stranded in a column nothing reads. The shape says which
/// flags flipped since the last sync, so a field flipped back moves its values
/// back even though the column they move into is already there.
fn add_field_columns(ctx: &AlterCtx, locale_config: &LocaleConfig) -> Result<()> {
    let specs = collect_column_specs(&ctx.def.fields, locale_config);
    let shape = LocaleShape::load(ctx.conn, ctx.slug, &specs)?;

    for spec in &specs {
        let expected_type = spec.ddl_type(ctx.conn);

        for plan in column_plans(&spec.col_name, spec.is_localized, locale_config)? {
            let created = add_field_column(ctx, &plan.name, expected_type, spec)?;

            if shape.must_carry(&spec.col_name, created) {
                plan.carry_values(ctx.conn, ctx.slug, ctx.existing)?;
            }
        }
    }

    shape.record(ctx.conn, ctx.slug)
}

/// Add a column to a table if it doesn't already exist.
fn ensure_column(ctx: &AlterCtx, col_def: &str) -> Result<()> {
    let col_name = col_def
        .split_whitespace()
        .next()
        .expect("static column definition");

    add_column_if_missing(ctx.conn, ctx.slug, col_name, col_def, ctx.existing)
}

/// Add system columns (_status, auth, timestamps) as needed.
fn add_system_columns(ctx: &AlterCtx) -> Result<()> {
    add_draft_columns(ctx)?;
    add_auth_columns(ctx)?;
    add_soft_delete_columns(ctx)?;
    add_ref_count_column(ctx)?;
    ensure_column(ctx, REVISION_COLUMN)?;
    add_timestamp_columns(ctx)?;

    Ok(())
}

/// Add _status column for versioned collections with drafts.
fn add_draft_columns(ctx: &AlterCtx) -> Result<()> {
    if ctx.def.has_drafts() {
        ensure_column(ctx, DRAFT_STATUS_COLUMN)?;
    }

    Ok(())
}

/// Add auth system columns (password, reset tokens, lock, session version, MFA).
fn add_auth_columns(ctx: &AlterCtx) -> Result<()> {
    if !ctx.def.is_auth_collection() {
        return Ok(());
    }

    for col in AUTH_COLUMNS {
        ensure_column(ctx, col)?;
    }

    if ctx
        .def
        .auth
        .as_ref()
        .is_some_and(Auth::requires_verify_email)
    {
        for col in VERIFY_EMAIL_COLUMNS {
            ensure_column(ctx, col)?;
        }
    }

    if ctx
        .def
        .auth
        .as_ref()
        .is_some_and(|a| a.mfa() != MfaMode::Off)
    {
        for col in MFA_COLUMNS {
            ensure_column(ctx, col)?;
        }
    }

    if ctx
        .def
        .auth
        .as_ref()
        .is_some_and(|a| a.mfa() == MfaMode::Totp)
    {
        for col in TOTP_COLUMNS {
            ensure_column(ctx, col)?;
        }
    }

    Ok(())
}

/// Add _`deleted_at` column for soft-delete collections.
fn add_soft_delete_columns(ctx: &AlterCtx) -> Result<()> {
    if ctx.def.soft_delete && !ctx.existing.contains("_deleted_at") {
        let col_def = format!("_deleted_at {}", ctx.conn.timestamp_column_type());
        ensure_column(ctx, &col_def)?;
    }

    Ok(())
}

/// Add _`ref_count` column for delete protection.
fn add_ref_count_column(ctx: &AlterCtx) -> Result<()> {
    ensure_column(ctx, REF_COUNT_COLUMN)
}

/// Add `created_at/updated_at` timestamp columns.
fn add_timestamp_columns(ctx: &AlterCtx) -> Result<()> {
    if !ctx.def.timestamps {
        return Ok(());
    }

    // No server DEFAULT here, unlike CREATE: SQLite forbids
    // `ALTER TABLE ... ADD COLUMN` with a NON-CONSTANT default (the timestamp
    // default is `strftime(...)`/`NOW()`), so the add-timestamps path must use
    // the plain column type. This is safe — every write binds
    // `created_at`/`updated_at` explicitly (`utc_now()`), so the absent server
    // default never surfaces.
    let ts_type = ctx.conn.timestamp_column_type();

    for col_name in ["created_at", "updated_at"] {
        let col_def = format!("{col_name} {ts_type}");
        ensure_column(ctx, &col_def)?;
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
    let mut names = HashSet::new();

    for spec in collect_column_specs(&def.fields, locale_config) {
        names.insert(spec.col_name.clone());

        if spec.is_localized {
            for locale in &locale_config.locales {
                match locale_column(&spec.col_name, locale) {
                    Ok(col) => {
                        names.insert(col);
                    }
                    Err(e) => {
                        warn!(
                            "Failed to build locale column name for {}.{}: {e}",
                            spec.col_name, locale
                        );
                    }
                }
            }
        }
    }

    names
}

pub(super) fn alter_collection_table(
    conn: &dyn DbConnection,
    slug: &str,
    def: &CollectionDefinition,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let column_types = get_table_column_types(conn, slug)?;
    let existing: HashSet<String> = column_types.keys().cloned().collect();

    check_no_trashed_rows(conn, slug, def, &existing)?;

    let pending = PendingConstraints::read(conn, slug, def, &existing, locale_config)?;

    let ctx = AlterCtx {
        conn,
        slug,
        def,
        existing: &existing,
        column_types: &column_types,
    };

    add_field_columns(&ctx, locale_config)?;
    add_system_columns(&ctx)?;

    // Warn about removed columns (SQLite can't DROP COLUMN easily)
    warn_orphan_columns(
        slug,
        &existing,
        &collect_expected_column_names(def, locale_config),
    );

    // After the columns are reconciled, so a rebuild copies into columns whose
    // stored type was just checked against the definition.
    pending.apply(conn, slug, def, locale_config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::collection::*;
    use crate::core::{FieldDefinition, FieldTab, FieldType};
    use crate::db::DbValue;
    use crate::db::migrate::collection::create::create_collection_table;
    use crate::db::migrate::collection::sync_collection_table;
    use crate::db::migrate::collection::test_helpers::*;
    use crate::db::migrate::helpers::get_table_columns;
    use crate::db::query::helpers::column_value;
    use serde_json::json;

    /// Regression: a has-many default added by ALTER became the column DEFAULT
    /// through the single-value coercion, so a number list stored nothing where
    /// a write of the same default stores the list.
    #[test]
    fn alter_adds_a_has_many_default_as_its_write_stores_it() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def1 = simple_collection("posts", vec![text_field("title")]);
        create_collection_table(&conn, "posts", &def1, &no_locale()).unwrap();

        let def2 = simple_collection(
            "posts",
            vec![
                text_field("title"),
                FieldDefinition::builder("scores", FieldType::Number)
                    .has_many(true)
                    .default_value(json!([1, 2]))
                    .build(),
            ],
        );
        alter_collection_table(&conn, "posts", &def2, &no_locale()).unwrap();

        conn.execute("INSERT INTO posts (id) VALUES ('a')", &[])
            .unwrap();
        let row = conn
            .query_one("SELECT scores FROM posts WHERE id = 'a'", &[])
            .unwrap()
            .unwrap();

        assert_eq!(
            row.opt_text_at(0).map_or(DbValue::Null, DbValue::Text),
            column_value(&def2.fields[1], &json!([1, 2]), None)
        );
    }

    /// Marking an existing field `localized` adds `title__en` beside the bare
    /// `title` the content sits in. Without carrying the values across, every
    /// read returns the new empty column and the content is unreachable — and
    /// a schema cleanup drops the bare column with it.
    #[test]
    fn turning_localization_on_keeps_existing_values_in_the_default_locale() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let shared = simple_collection("posts", vec![text_field("title")]);
        create_collection_table(&conn, "posts", &shared, &locale_en_de()).unwrap();
        conn.execute("INSERT INTO posts (id, title) VALUES ('p1', 'Hello')", &[])
            .unwrap();

        let localized = simple_collection("posts", vec![localized_field("title")]);
        alter_collection_table(&conn, "posts", &localized, &locale_en_de()).unwrap();

        let row = conn
            .query_one(
                "SELECT title__en, title__de FROM posts WHERE id = 'p1'",
                &[],
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            row.get_string("title__en").unwrap(),
            "Hello",
            "the default locale must keep the content"
        );
        assert!(
            row.opt_text_at(1).is_none(),
            "the other locales start untranslated"
        );
    }

    /// A checkbox, and a field with a `default_value`.
    fn defaulted_fields(localized: bool) -> Vec<FieldDefinition> {
        vec![
            FieldDefinition::builder("featured", FieldType::Checkbox)
                .localized(localized)
                .build(),
            FieldDefinition::builder("tagline", FieldType::Text)
                .default_value(json!("draft"))
                .localized(localized)
                .build(),
        ]
    }

    /// Regression: `ADD COLUMN ... DEFAULT x` backfills every existing row, so
    /// the column a flip moves values into is never empty for a checkbox (always
    /// `DEFAULT 0`) or any field with a `default_value` — the carry used to skip
    /// every row on that ground and left the content in the bare column a schema
    /// cleanup drops. Marking such fields `localized` must still move them.
    #[test]
    fn turning_localization_on_keeps_a_defaulted_columns_values() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let shared = simple_collection("posts", defaulted_fields(false));
        create_collection_table(&conn, "posts", &shared, &locale_en_de()).unwrap();
        conn.execute(
            "INSERT INTO posts (id, featured, tagline) VALUES ('p1', 1, 'Hello')",
            &[],
        )
        .unwrap();

        let localized = simple_collection("posts", defaulted_fields(true));
        alter_collection_table(&conn, "posts", &localized, &locale_en_de()).unwrap();

        let row = conn
            .query_one(
                "SELECT featured__en, tagline__en FROM posts WHERE id = 'p1'",
                &[],
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            row.get_i64("featured__en").unwrap(),
            1,
            "a checkbox's DEFAULT 0 backfill must not swallow the stored value"
        );
        assert_eq!(row.get_string("tagline__en").unwrap(), "Hello");
    }

    /// The mirror: clearing `localized` on the same defaulted fields moves the
    /// default locale's values back into the bare column, which the ALTER also
    /// created with the field's DEFAULT.
    #[test]
    fn turning_localization_off_reclaims_defaulted_values() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let localized = simple_collection("posts", defaulted_fields(true));
        create_collection_table(&conn, "posts", &localized, &locale_en_de()).unwrap();
        conn.execute(
            "INSERT INTO posts (id, featured__en, tagline__en) VALUES ('p1', 1, 'Hello')",
            &[],
        )
        .unwrap();

        let shared = simple_collection("posts", defaulted_fields(false));
        alter_collection_table(&conn, "posts", &shared, &locale_en_de()).unwrap();

        let row = conn
            .query_one("SELECT featured, tagline FROM posts WHERE id = 'p1'", &[])
            .unwrap()
            .unwrap();
        assert_eq!(row.get_i64("featured").unwrap(), 1);
        assert_eq!(row.get_string("tagline").unwrap(), "Hello");
    }

    /// Regression: the carry keyed on column creation, so flipping a field back
    /// found the bare column already there (the first flip left it behind),
    /// carried nothing, and the reads returned the value the field held BEFORE
    /// the first flip — every edit made while it was localized was lost to the
    /// reader. The recorded shape makes the second flip carry too.
    #[test]
    fn flipping_localization_back_keeps_the_edits_made_in_between() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let shared = simple_collection("posts", vec![text_field("title")]);
        let localized = simple_collection("posts", vec![localized_field("title")]);

        create_collection_table(&conn, "posts", &shared, &locale_en_de()).unwrap();
        conn.execute("INSERT INTO posts (id, title) VALUES ('p1', 'Hello')", &[])
            .unwrap();

        alter_collection_table(&conn, "posts", &localized, &locale_en_de()).unwrap();
        conn.execute("UPDATE posts SET title__en = 'Edited'", &[])
            .unwrap();

        alter_collection_table(&conn, "posts", &shared, &locale_en_de()).unwrap();

        let row = conn
            .query_one("SELECT title FROM posts WHERE id = 'p1'", &[])
            .unwrap()
            .unwrap();
        assert_eq!(
            row.get_string("title").unwrap(),
            "Edited",
            "the reads must follow the content, not the pre-flip copy"
        );
    }

    /// Clearing `localized` is the mirror: the bare column the reads return to
    /// takes the default locale's content back.
    #[test]
    fn turning_localization_off_reclaims_the_default_locale_values() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let localized = simple_collection("posts", vec![localized_field("title")]);
        create_collection_table(&conn, "posts", &localized, &locale_en_de()).unwrap();
        conn.execute(
            "INSERT INTO posts (id, title__en) VALUES ('p1', 'Hello')",
            &[],
        )
        .unwrap();

        let shared = simple_collection("posts", vec![text_field("title")]);
        alter_collection_table(&conn, "posts", &shared, &locale_en_de()).unwrap();

        let row = conn
            .query_one("SELECT title FROM posts WHERE id = 'p1'", &[])
            .unwrap()
            .unwrap();
        assert_eq!(row.get_string("title").unwrap(), "Hello");
    }

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
        def2.auth = Some(Auth::enabled().map_password_login(|b| b.verify_email(true)));
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
    fn alter_adds_deleted_at_for_soft_delete() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def1 = simple_collection("posts", vec![text_field("title")]);
        create_collection_table(&conn, "posts", &def1, &no_locale()).unwrap();

        // Enable soft delete on existing collection
        let mut def2 = simple_collection("posts", vec![text_field("title")]);
        def2.soft_delete = true;
        alter_collection_table(&conn, "posts", &def2, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("_deleted_at"));
    }

    #[test]
    fn alter_does_not_add_deleted_at_without_soft_delete() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def1 = simple_collection("posts", vec![text_field("title")]);
        create_collection_table(&conn, "posts", &def1, &no_locale()).unwrap();

        // Alter without soft_delete
        let def2 = simple_collection("posts", vec![text_field("title"), text_field("body")]);
        alter_collection_table(&conn, "posts", &def2, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(!cols.contains("_deleted_at"));
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

    /// Regression: locale-suffixed columns (e.g., `title__en`) must be recognized
    /// as expected when the field is localized. Previously they were falsely
    /// reported as orphans.
    #[test]
    fn localized_columns_not_flagged_as_orphans() {
        let locale = locale_en_de();
        let def = simple_collection("posts", vec![localized_field("title"), text_field("body")]);

        let expected = collect_expected_column_names(&def, &locale);

        assert!(expected.contains("title"), "base column");
        assert!(expected.contains("title__en"), "en locale column");
        assert!(expected.contains("title__de"), "de locale column");
        assert!(expected.contains("body"), "non-localized column");
        assert!(
            !expected.contains("body__en"),
            "non-localized should not have locale suffix"
        );
    }

    /// Regression: a field whose `type` changed only warned. The column kept
    /// the type it was created with, so on `SQLite` every number written into
    /// it was stored and read back as text, and on Postgres every later write
    /// failed to bind. The sync refuses to run instead, naming the column.
    #[test]
    fn a_changed_field_type_fails_the_sync() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();

        let as_text = simple_collection("posts", vec![text_field("score")]);
        sync_collection_table(&conn, "posts", &as_text, &no_locale()).unwrap();

        let as_number = simple_collection(
            "posts",
            vec![FieldDefinition::builder("score", FieldType::Number).build()],
        );
        let err = sync_collection_table(&conn, "posts", &as_number, &no_locale())
            .unwrap_err()
            .to_string();

        assert!(err.contains("score"), "must name the column: {err}");
        assert!(err.contains("posts"), "must name the table: {err}");
        assert!(err.contains("REAL"), "must name the expected type: {err}");
        assert!(err.contains("TEXT"), "must name the stored type: {err}");
    }

    /// The unchanged case stays a no-op — re-syncing the same definition must
    /// not trip the type check.
    #[test]
    fn an_unchanged_field_type_re_syncs_cleanly() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();

        let def = simple_collection(
            "posts",
            vec![
                text_field("title"),
                FieldDefinition::builder("rank", FieldType::Number).build(),
                FieldDefinition::builder("featured", FieldType::Checkbox).build(),
            ],
        );

        sync_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        sync_collection_table(&conn, "posts", &def, &no_locale())
            .expect("re-syncing an unchanged definition must pass");
    }
}
