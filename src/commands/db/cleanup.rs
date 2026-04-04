//! `db cleanup` subcommand: detect and remove orphan columns.

use std::path::Path;

use anyhow::{Context as _, Result, anyhow, bail};

use crate::{
    cli,
    config::{CrapConfig, LocaleConfig},
    core::Registry,
    db::{DbConnection, migrate, pool, query},
    hooks,
};

/// Detect and optionally remove orphan columns not present in Lua definitions.
///
/// Orphan columns are columns that exist in a collection table but do not correspond
/// to any field in the current Lua definition. System columns (`_`-prefixed) are
/// always kept. Because Lua definitions include plugin-added fields (plugins run
/// during `init_lua`), plugin columns are never flagged as orphans.
///
/// By default runs in dry-run mode (report only). Pass `confirm = true` to actually
/// drop orphan columns.
#[cfg(not(tarpaulin_include))]
pub fn cleanup(config_dir: &Path, confirm: bool) -> Result<()> {
    let config_dir = config_dir
        .canonicalize()
        .unwrap_or_else(|_| config_dir.to_path_buf());

    let cfg = CrapConfig::load(&config_dir).context("Failed to load config")?;
    let registry = hooks::init_lua(&config_dir, &cfg).context("Failed to initialize Lua VM")?;
    let pool = pool::create_pool(&config_dir, &cfg).context("Failed to create database pool")?;

    migrate::sync_all(&pool, &registry, &cfg.locale).context("Failed to sync database schema")?;

    let reg = registry
        .read()
        .map_err(|e| anyhow!("Registry lock poisoned: {}", e))?;

    let conn = pool.get().context("Failed to get database connection")?;
    let orphans = find_orphan_columns(&conn as &dyn DbConnection, &reg, &cfg.locale)?;

    if orphans.is_empty() {
        cli::success("No orphan columns found. All columns match Lua definitions.");
        return Ok(());
    }

    display_orphans(&orphans);

    if !confirm {
        cli::hint("This is a dry run. Pass --confirm to drop these columns.");
        cli::hint("Note: dropping columns is irreversible. Back up your database first.");
        return Ok(());
    }

    drop_orphan_columns(&conn, &orphans)
}

/// Display the list of orphan columns found.
fn display_orphans(orphans: &[(String, Vec<String>)]) {
    cli::warning("Orphan columns (not in Lua definitions):");
    println!();

    for (table, cols) in orphans {
        for col in cols {
            cli::dim(&format!("  {}.{}", table, col));
        }
    }

    let total: usize = orphans.iter().map(|(_, cols)| cols.len()).sum();

    println!();
    cli::info(&format!("{} orphan column(s) found.", total));
}

/// Drop the identified orphan columns from the database.
fn drop_orphan_columns(conn: &dyn DbConnection, orphans: &[(String, Vec<String>)]) -> Result<()> {
    if !conn.supports_drop_column() {
        bail!(
            "Database does not support DROP COLUMN. \
             Consider recreating the table manually."
        );
    }

    let mut total = 0;

    for (table, cols) in orphans {
        for col in cols {
            let sql = format!("ALTER TABLE \"{}\" DROP COLUMN \"{}\"", table, col);

            conn.execute(&sql, &[])
                .with_context(|| format!("Failed to drop column {}.{}", table, col))?;

            cli::success(&format!("Dropped: {}.{}", table, col));
            total += 1;
        }
    }

    cli::success(&format!("{} column(s) dropped.", total));

    Ok(())
}

/// Find orphan columns across all collection tables.
///
/// Returns a vec of (table_name, vec_of_orphan_column_names).
/// System columns (`_`-prefixed, `id`, `created_at`, `updated_at`) are excluded.
/// Plugin columns are NOT orphans because plugins run during `init_lua` and their
/// fields are included in the registry definitions.
pub fn find_orphan_columns(
    conn: &dyn DbConnection,
    reg: &Registry,
    locale_config: &LocaleConfig,
) -> Result<Vec<(String, Vec<String>)>> {
    let mut results = Vec::new();
    let mut slugs: Vec<_> = reg.collections.keys().collect();

    slugs.sort();

    for slug in slugs {
        let def = &reg.collections[slug];
        let existing = migrate::helpers::get_table_columns(conn, slug)?;

        if existing.is_empty() {
            continue;
        }

        let expected = query::get_expected_column_names(def, locale_config);

        let mut orphan_cols: Vec<String> = existing
            .iter()
            .filter(|col| !expected.contains(*col) && !col.starts_with('_'))
            .cloned()
            .collect();

        if !orphan_cols.is_empty() {
            orphan_cols.sort();
            results.push((slug.to_string(), orphan_cols));
        }
    }

    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        core::{
            collection::CollectionDefinition,
            field::{FieldDefinition, FieldType},
        },
        db::{BoxedConnection, pool},
    };
    use tempfile::TempDir;

    fn no_locale() -> LocaleConfig {
        LocaleConfig::default()
    }

    fn locale_en_de() -> LocaleConfig {
        LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: true,
        }
    }

    fn simple_collection(slug: &str, fields: Vec<FieldDefinition>) -> CollectionDefinition {
        let mut def = CollectionDefinition::new(slug);
        def.timestamps = true;
        def.fields = fields;
        def
    }

    fn text_field(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Text).build()
    }

    fn make_conn() -> (TempDir, BoxedConnection) {
        let dir = TempDir::new().unwrap();
        let cfg = CrapConfig::default();
        let p = pool::create_pool(dir.path(), &cfg).unwrap();
        let conn = p.get().unwrap();
        (dir, conn)
    }

    #[test]
    fn no_orphans_when_columns_match() {
        let (_dir, conn) = make_conn();
        conn.execute_batch(
            "CREATE TABLE posts (id TEXT, title TEXT, created_at TEXT, updated_at TEXT)",
        )
        .unwrap();

        let mut reg = Registry::default();
        reg.collections.insert(
            "posts".into(),
            simple_collection("posts", vec![text_field("title")]),
        );

        let orphans = find_orphan_columns(&conn, &reg, &no_locale()).unwrap();
        assert!(orphans.is_empty());
    }

    #[test]
    fn detects_orphan_column() {
        let (_dir, conn) = make_conn();
        conn.execute_batch(
            "CREATE TABLE posts (id TEXT, title TEXT, old_field TEXT, created_at TEXT, updated_at TEXT)",
        )
        .unwrap();

        let mut reg = Registry::default();
        reg.collections.insert(
            "posts".into(),
            simple_collection("posts", vec![text_field("title")]),
        );

        let orphans = find_orphan_columns(&conn, &reg, &no_locale()).unwrap();
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0].0, "posts");
        assert_eq!(orphans[0].1, vec!["old_field"]);
    }

    #[test]
    fn system_columns_not_orphans() {
        let (_dir, conn) = make_conn();
        conn.execute_batch(
            "CREATE TABLE users (id TEXT, email TEXT, _password_hash TEXT, _locked INTEGER, created_at TEXT, updated_at TEXT)",
        )
        .unwrap();

        let mut reg = Registry::default();
        reg.collections.insert(
            "users".into(),
            simple_collection("users", vec![text_field("email")]),
        );

        let orphans = find_orphan_columns(&conn, &reg, &no_locale()).unwrap();
        assert!(orphans.is_empty(), "system columns should not be flagged");
    }

    #[test]
    fn group_fields_not_orphans() {
        let (_dir, conn) = make_conn();
        conn.execute_batch(
            "CREATE TABLE posts (id TEXT, seo__meta_title TEXT, seo__meta_desc TEXT, created_at TEXT, updated_at TEXT)",
        )
        .unwrap();

        let mut reg = Registry::default();
        reg.collections.insert(
            "posts".into(),
            simple_collection(
                "posts",
                vec![
                    FieldDefinition::builder("seo", FieldType::Group)
                        .fields(vec![text_field("meta_title"), text_field("meta_desc")])
                        .build(),
                ],
            ),
        );

        let orphans = find_orphan_columns(&conn, &reg, &no_locale()).unwrap();
        assert!(orphans.is_empty(), "group fields should not be flagged");
    }

    #[test]
    fn localized_columns_not_orphans() {
        let (_dir, conn) = make_conn();
        conn.execute_batch(
            "CREATE TABLE posts (id TEXT, title__en TEXT, title__de TEXT, created_at TEXT, updated_at TEXT)",
        )
        .unwrap();

        let mut reg = Registry::default();
        reg.collections.insert(
            "posts".into(),
            simple_collection(
                "posts",
                vec![
                    FieldDefinition::builder("title", FieldType::Text)
                        .localized(true)
                        .build(),
                ],
            ),
        );

        let orphans = find_orphan_columns(&conn, &reg, &locale_en_de()).unwrap();
        assert!(orphans.is_empty());
    }

    #[test]
    fn detects_orphan_among_valid_columns() {
        let (_dir, conn) = make_conn();
        conn.execute_batch(
            "CREATE TABLE posts (id TEXT, title TEXT, removed_field TEXT, seo__meta TEXT, created_at TEXT, updated_at TEXT)",
        )
        .unwrap();

        let mut reg = Registry::default();
        reg.collections.insert(
            "posts".into(),
            simple_collection(
                "posts",
                vec![
                    text_field("title"),
                    FieldDefinition::builder("seo", FieldType::Group)
                        .fields(vec![text_field("meta")])
                        .build(),
                ],
            ),
        );

        let orphans = find_orphan_columns(&conn, &reg, &no_locale()).unwrap();
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0].1, vec!["removed_field"]);
    }

    #[test]
    fn nested_group_in_row_in_tabs_not_orphans() {
        let (_dir, conn) = make_conn();
        conn.execute_batch(
            "CREATE TABLE posts (id TEXT, seo__title TEXT, body TEXT, created_at TEXT, updated_at TEXT)",
        )
        .unwrap();

        let mut reg = Registry::default();
        reg.collections.insert(
            "posts".into(),
            simple_collection(
                "posts",
                vec![
                    FieldDefinition::builder("layout", FieldType::Tabs)
                        .tabs(vec![crate::core::field::FieldTab::new(
                            "Content",
                            vec![
                                FieldDefinition::builder("row", FieldType::Row)
                                    .fields(vec![
                                        FieldDefinition::builder("seo", FieldType::Group)
                                            .fields(vec![text_field("title")])
                                            .build(),
                                        text_field("body"),
                                    ])
                                    .build(),
                            ],
                        )])
                        .build(),
                ],
            ),
        );

        let orphans = find_orphan_columns(&conn, &reg, &no_locale()).unwrap();
        assert!(
            orphans.is_empty(),
            "nested Group→Row→Tabs columns should not be orphans: {:?}",
            orphans
        );
    }
}
