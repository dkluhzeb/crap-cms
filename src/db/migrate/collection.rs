//! Collection table sync: create and alter collection tables from Lua definitions.

use anyhow::{Context, Result};
use std::collections::HashSet;

use crate::config::LocaleConfig;
use crate::core::field::FieldType;

use super::helpers::{table_exists, get_table_columns, collect_column_specs, sync_join_tables, sync_versions_table};

pub(super) fn sync_collection_table(
    conn: &rusqlite::Connection,
    slug: &str,
    def: &crate::core::CollectionDefinition,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let table_exists = table_exists(conn, slug)?;

    if !table_exists {
        create_collection_table(conn, slug, def, locale_config)?;
    } else {
        alter_collection_table(conn, slug, def, locale_config)?;
    }

    // Sync join tables for has-many relationships and array fields
    sync_join_tables(conn, slug, &def.fields, locale_config)?;

    // Sync versions table if versioning is enabled
    if def.has_versions() {
        sync_versions_table(conn, slug)?;
    }

    // Sync FTS5 full-text search index
    crate::db::query::fts::sync_fts_table(conn, slug, def, locale_config)?;

    // Sync B-tree indexes (field-level index=true + collection-level compound indexes)
    sync_indexes(conn, slug, def, locale_config)?;

    Ok(())
}

pub(super) fn create_collection_table(
    conn: &rusqlite::Connection,
    slug: &str,
    def: &crate::core::CollectionDefinition,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let mut columns = vec!["id TEXT PRIMARY KEY".to_string()];

    for spec in &super::helpers::collect_column_specs(&def.fields, locale_config) {
        if spec.is_localized {
            for locale in &locale_config.locales {
                let col_name = format!("{}__{}", spec.col_name, locale);
                let mut col = format!("{} {}", col_name, spec.field.field_type.sqlite_type());
                if spec.field.required && *locale == locale_config.default_locale && !def.has_drafts() {
                    col.push_str(" NOT NULL");
                }
                if spec.field.unique {
                    col.push_str(" UNIQUE");
                }
                append_default_value(&mut col, &spec.field.default_value, &spec.field.field_type);
                columns.push(col);
            }
        } else {
            let mut col = format!("{} {}", spec.col_name, spec.field.field_type.sqlite_type());
            if spec.field.required && !def.has_drafts() {
                col.push_str(" NOT NULL");
            }
            if spec.field.unique {
                col.push_str(" UNIQUE");
            }
            append_default_value(&mut col, &spec.field.default_value, &spec.field.field_type);
            columns.push(col);
        }
    }

    // Versioned collections with drafts get a _status column
    if def.has_drafts() {
        columns.push("_status TEXT NOT NULL DEFAULT 'published'".to_string());
    }

    // Auth collections get hidden system columns (not regular fields)
    if def.is_auth_collection() {
        columns.push("_password_hash TEXT".to_string());
        columns.push("_reset_token TEXT".to_string());
        columns.push("_reset_token_exp INTEGER".to_string());
        columns.push("_locked INTEGER DEFAULT 0".to_string());
        columns.push("_settings TEXT".to_string());
        if def.auth.as_ref().is_some_and(|a| a.verify_email) {
            columns.push("_verified INTEGER DEFAULT 0".to_string());
            columns.push("_verification_token TEXT".to_string());
            columns.push("_verification_token_exp INTEGER".to_string());
        }
    }

    if def.timestamps {
        columns.push("created_at TEXT DEFAULT (datetime('now'))".to_string());
        columns.push("updated_at TEXT DEFAULT (datetime('now'))".to_string());
    }

    let sql = format!("CREATE TABLE {} ({})", slug, columns.join(", "));

    tracing::info!("Creating collection table: {}", slug);
    tracing::debug!("SQL: {}", sql);

    conn.execute(&sql, [])
        .with_context(|| format!("Failed to create table {}", slug))?;

    Ok(())
}

fn alter_collection_table(
    conn: &rusqlite::Connection,
    slug: &str,
    def: &crate::core::CollectionDefinition,
    locale_config: &LocaleConfig,
) -> Result<()> {
    // Get existing columns
    let existing_columns = get_table_columns(conn, slug)?;

    for spec in &super::helpers::collect_column_specs(&def.fields, locale_config) {
        if spec.is_localized {
            for locale in &locale_config.locales {
                let col_name = format!("{}__{}", spec.col_name, locale);
                if !existing_columns.contains(&col_name) {
                    let mut col_def = spec.field.field_type.sqlite_type().to_string();
                    append_default_value(&mut col_def, &spec.field.default_value, &spec.field.field_type);
                    let sql = format!("ALTER TABLE {} ADD COLUMN {} {}", slug, col_name, col_def);
                    tracing::info!("Adding column to {}: {}", slug, col_name);
                    conn.execute(&sql, [])
                        .with_context(|| format!("Failed to add column {} to {}", col_name, slug))?;
                }
            }
        } else if !existing_columns.contains(&spec.col_name) {
            let mut col_def = spec.field.field_type.sqlite_type().to_string();
            append_default_value(&mut col_def, &spec.field.default_value, &spec.field.field_type);
            let sql = format!("ALTER TABLE {} ADD COLUMN {} {}", slug, spec.col_name, col_def);
            tracing::info!("Adding column to {}: {}", slug, spec.col_name);
            conn.execute(&sql, [])
                .with_context(|| format!("Failed to add column {} to {}", spec.col_name, slug))?;
        }
    }

    // Versioned collections with drafts: ensure _status column exists
    if def.has_drafts() && !existing_columns.contains("_status") {
        let sql = format!("ALTER TABLE {} ADD COLUMN _status TEXT NOT NULL DEFAULT 'published'", slug);
        tracing::info!("Adding _status column to {}", slug);
        conn.execute(&sql, [])
            .with_context(|| format!("Failed to add _status to {}", slug))?;
    }

    // Auth collections: ensure system columns exist
    if def.is_auth_collection() {
        for col in ["_password_hash TEXT", "_reset_token TEXT", "_reset_token_exp INTEGER", "_locked INTEGER DEFAULT 0", "_settings TEXT"] {
            let col_name = col.split_whitespace().next().expect("static column definition");
            if !existing_columns.contains(col_name) {
                let sql = format!("ALTER TABLE {} ADD COLUMN {}", slug, col);
                tracing::info!("Adding {} column to {}", col_name, slug);
                conn.execute(&sql, [])
                    .with_context(|| format!("Failed to add {} to {}", col_name, slug))?;
            }
        }
        if def.auth.as_ref().is_some_and(|a| a.verify_email) {
            for col in ["_verified INTEGER DEFAULT 0", "_verification_token TEXT", "_verification_token_exp INTEGER"] {
                let col_name = col.split_whitespace().next().expect("static column definition");
                if !existing_columns.contains(col_name) {
                    let sql = format!("ALTER TABLE {} ADD COLUMN {}", slug, col);
                    tracing::info!("Adding {} column to {}", col_name, slug);
                    conn.execute(&sql, [])
                        .with_context(|| format!("Failed to add {} to {}", col_name, slug))?;
                }
            }
        }
    }

    // Timestamps: ensure created_at/updated_at exist when timestamps are enabled
    // Note: SQLite ALTER TABLE cannot use non-constant defaults like datetime('now'),
    // so we add with no default (NULL for existing rows) — new inserts set these explicitly.
    if def.timestamps {
        for col_name in ["created_at", "updated_at"] {
            if !existing_columns.contains(col_name) {
                let sql = format!("ALTER TABLE {} ADD COLUMN {} TEXT", slug, col_name);
                tracing::info!("Adding {} column to {}", col_name, slug);
                conn.execute(&sql, [])
                    .with_context(|| format!("Failed to add {} to {}", col_name, slug))?;
            }
        }
    }

    // Warn about removed columns (SQLite can't DROP COLUMN easily)
    let mut field_names: HashSet<String> = HashSet::new();
    for f in &def.fields {
        if f.field_type == FieldType::Group {
            for sub in &f.fields {
                let base = format!("{}__{}", f.name, sub.name);
                let is_localized = (f.localized || sub.localized) && locale_config.is_enabled();
                if is_localized {
                    for locale in &locale_config.locales {
                        field_names.insert(format!("{}__{}", base, locale));
                    }
                } else {
                    field_names.insert(base);
                }
            }
        } else if f.field_type == FieldType::Row || f.field_type == FieldType::Collapsible {
            for sub in &f.fields {
                let is_localized = sub.localized && locale_config.is_enabled();
                if is_localized {
                    for locale in &locale_config.locales {
                        field_names.insert(format!("{}__{}", sub.name, locale));
                    }
                } else {
                    field_names.insert(sub.name.clone());
                }
            }
        } else if f.field_type == FieldType::Tabs {
            for tab in &f.tabs {
                for sub in &tab.fields {
                    let is_localized = sub.localized && locale_config.is_enabled();
                    if is_localized {
                        for locale in &locale_config.locales {
                            field_names.insert(format!("{}__{}", sub.name, locale));
                        }
                    } else {
                        field_names.insert(sub.name.clone());
                    }
                }
            }
        } else if f.has_parent_column() {
            if f.localized && locale_config.is_enabled() {
                for locale in &locale_config.locales {
                    field_names.insert(format!("{}__{}", f.name, locale));
                }
            } else {
                field_names.insert(f.name.clone());
            }
        }
    }
    let system_columns: HashSet<&str> = [
        "id", "created_at", "updated_at", "_password_hash",
        "_reset_token", "_reset_token_exp", "_verified", "_verification_token",
        "_verification_token_exp", "_locked", "_status", "_settings",
    ].into();
    for col in &existing_columns {
        if !field_names.contains(col) && !system_columns.contains(col.as_str()) {
            tracing::warn!(
                "Column '{}' exists in table '{}' but not in Lua definition (not removed)",
                col, slug
            );
        }
    }

    Ok(())
}

/// Append a DEFAULT value clause to a column definition string.
fn append_default_value(col: &mut String, default_value: &Option<serde_json::Value>, field_type: &FieldType) {
    if let Some(ref default) = default_value {
        match default {
            serde_json::Value::String(s) => col.push_str(&format!(" DEFAULT '{}'", s.replace('\'', "''"))),
            serde_json::Value::Number(n) => col.push_str(&format!(" DEFAULT {}", n)),
            serde_json::Value::Bool(b) => col.push_str(&format!(" DEFAULT {}", if *b { 1 } else { 0 })),
            _ => {}
        }
    } else if *field_type == FieldType::Checkbox {
        col.push_str(" DEFAULT 0");
    }
}

/// Sync B-tree indexes for a collection table: field-level `index: true` and
/// collection-level compound `indexes`. Idempotent — creates missing indexes,
/// drops stale ones. Only manages indexes with the `idx_{slug}_` naming prefix.
pub(super) fn sync_indexes(
    conn: &rusqlite::Connection,
    slug: &str,
    def: &crate::core::CollectionDefinition,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let mut desired: HashSet<String> = HashSet::new();
    let mut create_stmts: Vec<String> = Vec::new();

    // 1. Field-level indexes: index=true (skip if unique=true — SQLite already indexes those)
    for spec in &collect_column_specs(&def.fields, locale_config) {
        if !spec.field.index || spec.field.unique {
            continue;
        }
        if spec.is_localized {
            for locale in &locale_config.locales {
                let col = format!("{}__{}", spec.col_name, locale);
                let idx_name = format!("idx_{}_{}", slug, col);
                create_stmts.push(format!(
                    "CREATE INDEX IF NOT EXISTS {} ON {} ({})",
                    idx_name, slug, col
                ));
                desired.insert(idx_name);
            }
        } else {
            let idx_name = format!("idx_{}_{}", slug, spec.col_name);
            create_stmts.push(format!(
                "CREATE INDEX IF NOT EXISTS {} ON {} ({})",
                idx_name, slug, spec.col_name
            ));
            desired.insert(idx_name);
        }
    }

    // 2. Collection-level compound indexes
    for index_def in &def.indexes {
        // Validate all field names
        for field_name in &index_def.fields {
            if !crate::db::query::is_valid_identifier(field_name) {
                anyhow::bail!(
                    "Invalid field name '{}' in compound index for collection '{}'",
                    field_name, slug
                );
            }
        }

        // Expand localized fields to locale-specific columns
        let specs = collect_column_specs(&def.fields, locale_config);
        let mut expanded_cols: Vec<String> = Vec::new();
        for field_name in &index_def.fields {
            // Find the matching column spec to check if it's localized
            let spec = specs.iter().find(|s| s.col_name == *field_name);
            match spec {
                Some(s) if s.is_localized => {
                    // For localized fields in compound indexes, use default locale column
                    expanded_cols.push(format!("{}__{}", field_name, locale_config.default_locale));
                }
                _ => {
                    expanded_cols.push(field_name.clone());
                }
            }
        }

        let col_list = expanded_cols.join(", ");
        let name_suffix = index_def.fields.join("_");
        let idx_name = format!("idx_{}_{}", slug, name_suffix);
        let unique = if index_def.unique { "UNIQUE " } else { "" };
        create_stmts.push(format!(
            "CREATE {}INDEX IF NOT EXISTS {} ON {} ({})",
            unique, idx_name, slug, col_list
        ));
        desired.insert(idx_name);
    }

    // 3. Get existing managed indexes (our prefix only)
    let prefix = format!("idx_{}_", slug);
    let mut existing: HashSet<String> = HashSet::new();
    let mut stmt = conn.prepare(
        "SELECT name FROM sqlite_master WHERE type='index' AND tbl_name=?1 AND name LIKE ?2"
    )?;
    let rows = stmt.query_map(rusqlite::params![slug, format!("{}%", prefix)], |row| {
        row.get::<_, String>(0)
    })?;
    for row in rows {
        existing.insert(row?);
    }

    // 4. Drop stale indexes (in existing but not in desired)
    for name in existing.difference(&desired) {
        tracing::info!("Dropping stale index: {}", name);
        conn.execute(&format!("DROP INDEX IF EXISTS {}", name), [])
            .with_context(|| format!("Failed to drop index {}", name))?;
    }

    // 5. Create missing indexes
    for stmt_sql in &create_stmts {
        conn.execute(stmt_sql, [])
            .with_context(|| format!("Failed to create index: {}", stmt_sql))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LocaleConfig;
    use crate::core::collection::*;
    use crate::core::field::{FieldDefinition, FieldType};
    use crate::db::DbPool;

    fn in_memory_pool() -> DbPool {
        let manager = r2d2_sqlite::SqliteConnectionManager::memory()
            .with_flags(rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
                | rusqlite::OpenFlags::SQLITE_OPEN_CREATE
                | rusqlite::OpenFlags::SQLITE_OPEN_FULL_MUTEX
                | rusqlite::OpenFlags::SQLITE_OPEN_SHARED_CACHE);
        r2d2::Pool::builder()
            .max_size(2)
            .build(manager)
            .expect("in-memory pool")
    }

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
        CollectionDefinition {
            slug: slug.to_string(),
            labels: CollectionLabels::default(),
            timestamps: true,
            fields,
            admin: CollectionAdmin::default(),
            hooks: CollectionHooks::default(),
            auth: None,
            upload: None,
            access: CollectionAccess::default(),
            mcp: Default::default(),
            live: None,
            versions: None,
            indexes: Vec::new(),
        }
    }

    fn text_field(name: &str) -> FieldDefinition {
        FieldDefinition {
            name: name.to_string(),
            field_type: FieldType::Text,
            ..Default::default()
        }
    }

    fn localized_field(name: &str) -> FieldDefinition {
        FieldDefinition {
            name: name.to_string(),
            field_type: FieldType::Text,
            localized: true,
            ..Default::default()
        }
    }

    // ── create_collection_table ──────────────────────────────────────────

    #[test]
    fn create_simple_collection_table() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![
            text_field("title"),
            text_field("body"),
        ]);
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        assert!(table_exists(&conn, "posts").unwrap());
        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("id"));
        assert!(cols.contains("title"));
        assert!(cols.contains("body"));
        assert!(cols.contains("created_at"));
        assert!(cols.contains("updated_at"));
    }

    // ── alter adds new column ─────────────────────────────────────────────

    #[test]
    fn alter_adds_new_column() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def1 = simple_collection("posts", vec![text_field("title")]);
        create_collection_table(&conn, "posts", &def1, &no_locale()).unwrap();

        let def2 = simple_collection("posts", vec![
            text_field("title"),
            text_field("summary"),
        ]);
        alter_collection_table(&conn, "posts", &def2, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("summary"), "new column should be added");
    }

    // ── localized columns ─────────────────────────────────────────────────

    #[test]
    fn create_with_localized_fields() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![localized_field("title")]);
        create_collection_table(&conn, "posts", &def, &locale_en_de()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("title__en"), "should have en locale column");
        assert!(cols.contains("title__de"), "should have de locale column");
        assert!(!cols.contains("title"), "should NOT have bare title column");
    }

    // ── auth collection columns ───────────────────────────────────────────

    #[test]
    fn create_auth_collection_has_system_columns() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let mut def = simple_collection("users", vec![text_field("email")]);
        def.auth = Some(CollectionAuth {
            enabled: true,
            verify_email: true,
            ..Default::default()
        });
        create_collection_table(&conn, "users", &def, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "users").unwrap();
        assert!(cols.contains("_password_hash"));
        assert!(cols.contains("_reset_token"));
        assert!(cols.contains("_reset_token_exp"));
        assert!(cols.contains("_locked"));
        assert!(cols.contains("_settings"));
        assert!(cols.contains("_verified"));
        assert!(cols.contains("_verification_token"));
    }

    // ── versioned collection ──────────────────────────────────────────────

    #[test]
    fn versioned_collection_creates_versions_table() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let mut def = simple_collection("posts", vec![text_field("title")]);
        def.versions = Some(VersionsConfig { drafts: true, max_versions: 10 });
        sync_collection_table(&conn, "posts", &def, &no_locale()).unwrap();

        assert!(table_exists(&conn, "_versions_posts").unwrap());
        let cols = get_table_columns(&conn, "_versions_posts").unwrap();
        assert!(cols.contains("_parent"));
        assert!(cols.contains("_version"));
        assert!(cols.contains("_status"));
        assert!(cols.contains("_latest"));
        assert!(cols.contains("snapshot"));
    }

    // ── drafts adds _status column ────────────────────────────────────────

    #[test]
    fn drafts_collection_has_status_column() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let mut def = simple_collection("posts", vec![text_field("title")]);
        def.versions = Some(VersionsConfig { drafts: true, max_versions: 0 });
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("_status"));
    }

    // ── group fields ──────────────────────────────────────────────────────

    #[test]
    fn group_field_creates_prefixed_columns() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![
            FieldDefinition {
                name: "seo".to_string(),
                field_type: FieldType::Group,
                fields: vec![text_field("meta_title"), text_field("meta_desc")],
                ..Default::default()
            },
        ]);
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("seo__meta_title"));
        assert!(cols.contains("seo__meta_desc"));
        assert!(!cols.contains("seo"), "group field itself should not be a column");
    }

    // ── alter adds auth system columns ──────────────────────────────────

    #[test]
    fn alter_adds_auth_system_columns() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def1 = simple_collection("users", vec![text_field("email")]);
        create_collection_table(&conn, "users", &def1, &no_locale()).unwrap();

        // Now make it an auth collection with verify_email
        let mut def2 = simple_collection("users", vec![text_field("email")]);
        def2.auth = Some(CollectionAuth {
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
        assert!(cols.contains("_verified"));
        assert!(cols.contains("_verification_token"));
    }

    // ── alter adds _status for drafts ───────────────────────────────────

    #[test]
    fn alter_adds_status_for_drafts() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def1 = simple_collection("posts", vec![text_field("title")]);
        create_collection_table(&conn, "posts", &def1, &no_locale()).unwrap();

        // Enable drafts on existing collection
        let mut def2 = simple_collection("posts", vec![text_field("title")]);
        def2.versions = Some(VersionsConfig { drafts: true, max_versions: 5 });
        alter_collection_table(&conn, "posts", &def2, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("_status"));
    }

    // ── alter adds timestamps to existing table ─────────────────────────

    #[test]
    fn alter_adds_timestamps() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        // Create a table without timestamps
        conn.execute("CREATE TABLE posts (id TEXT PRIMARY KEY, title TEXT)", []).unwrap();

        let def = simple_collection("posts", vec![text_field("title")]);
        alter_collection_table(&conn, "posts", &def, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("created_at"));
        assert!(cols.contains("updated_at"));
    }

    // ── alter warns about removed columns ───────────────────────────────

    #[test]
    fn alter_collection_with_localized_fields() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![localized_field("title")]);
        create_collection_table(&conn, "posts", &def, &locale_en_de()).unwrap();

        // Add a new localized field via alter
        let def2 = simple_collection("posts", vec![
            localized_field("title"),
            localized_field("body"),
        ]);
        alter_collection_table(&conn, "posts", &def2, &locale_en_de()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("body__en"));
        assert!(cols.contains("body__de"));
    }

    // ── alter group fields on collection ────────────────────────────────

    #[test]
    fn alter_adds_group_fields() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def1 = simple_collection("posts", vec![text_field("title")]);
        create_collection_table(&conn, "posts", &def1, &no_locale()).unwrap();

        let def2 = simple_collection("posts", vec![
            text_field("title"),
            FieldDefinition {
                name: "seo".to_string(),
                field_type: FieldType::Group,
                fields: vec![text_field("meta_title"), text_field("meta_desc")],
                ..Default::default()
            },
        ]);
        alter_collection_table(&conn, "posts", &def2, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("seo__meta_title"));
        assert!(cols.contains("seo__meta_desc"));
    }

    // ── alter localized group fields on collection ──────────────────────

    #[test]
    fn alter_adds_localized_group_fields() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def1 = simple_collection("posts", vec![text_field("title")]);
        create_collection_table(&conn, "posts", &def1, &locale_en_de()).unwrap();

        let def2 = simple_collection("posts", vec![
            text_field("title"),
            FieldDefinition {
                name: "seo".to_string(),
                field_type: FieldType::Group,
                localized: true,
                fields: vec![text_field("meta_title")],
                ..Default::default()
            },
        ]);
        alter_collection_table(&conn, "posts", &def2, &locale_en_de()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("seo__meta_title__en"));
        assert!(cols.contains("seo__meta_title__de"));
    }

    // ── create_collection_table with default values ─────────────────────

    #[test]
    fn create_with_default_values() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![
            FieldDefinition {
                name: "status".to_string(),
                field_type: FieldType::Text,
                default_value: Some(serde_json::json!("draft")),
                ..Default::default()
            },
            FieldDefinition {
                name: "count".to_string(),
                field_type: FieldType::Number,
                default_value: Some(serde_json::json!(0)),
                ..Default::default()
            },
        ]);
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        // Just verify it was created (defaults encoded in DDL)
        assert!(table_exists(&conn, "posts").unwrap());
    }

    // ── create_collection_table with required + unique fields ────────────

    #[test]
    fn create_with_required_unique_fields() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![
            FieldDefinition {
                name: "slug".to_string(),
                field_type: FieldType::Text,
                required: true,
                unique: true,
                ..Default::default()
            },
        ]);
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        assert!(table_exists(&conn, "posts").unwrap());
    }

    // ── create_collection with no timestamps ────────────────────────────

    #[test]
    fn create_collection_no_timestamps() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let mut def = simple_collection("posts", vec![text_field("title")]);
        def.timestamps = false;
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(!cols.contains("created_at"));
        assert!(!cols.contains("updated_at"));
    }

    // ── create_collection with localized group sub-field ────────────────

    #[test]
    fn create_localized_group_subfield() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![
            FieldDefinition {
                name: "seo".to_string(),
                field_type: FieldType::Group,
                fields: vec![
                    FieldDefinition {
                        name: "title".to_string(),
                        field_type: FieldType::Text,
                        localized: true,
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        ]);
        create_collection_table(&conn, "posts", &def, &locale_en_de()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("seo__title__en"));
        assert!(cols.contains("seo__title__de"));
    }

    // ── create_collection with required localized field on default locale ─

    #[test]
    fn create_required_localized_field() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![
            FieldDefinition {
                name: "title".to_string(),
                field_type: FieldType::Text,
                localized: true,
                required: true,
                unique: true,
                ..Default::default()
            },
        ]);
        create_collection_table(&conn, "posts", &def, &locale_en_de()).unwrap();

        // Should succeed — NOT NULL only on default locale
        assert!(table_exists(&conn, "posts").unwrap());
    }

    // ── create_collection with required localized group sub-field ────────

    #[test]
    fn create_required_localized_group_subfield() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![
            FieldDefinition {
                name: "seo".to_string(),
                field_type: FieldType::Group,
                localized: true,
                fields: vec![
                    FieldDefinition {
                        name: "title".to_string(),
                        field_type: FieldType::Text,
                        required: true,
                        unique: true,
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        ]);
        create_collection_table(&conn, "posts", &def, &locale_en_de()).unwrap();
        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("seo__title__en"));
        assert!(cols.contains("seo__title__de"));
    }

    // ── row fields (no prefix) ─────────────────────────────────────────────

    #[test]
    fn row_field_promotes_sub_fields_without_prefix() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![
            FieldDefinition {
                name: "layout".to_string(),
                field_type: FieldType::Row,
                fields: vec![text_field("first_name"), text_field("last_name")],
                ..Default::default()
            },
        ]);
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("first_name"), "Row sub-field should be a top-level column");
        assert!(cols.contains("last_name"), "Row sub-field should be a top-level column");
        assert!(!cols.contains("layout"), "Row field itself should not be a column");
        assert!(!cols.contains("layout__first_name"), "Row should not use prefix");
    }

    #[test]
    fn alter_adds_row_sub_fields() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def1 = simple_collection("posts", vec![text_field("title")]);
        create_collection_table(&conn, "posts", &def1, &no_locale()).unwrap();

        let def2 = simple_collection("posts", vec![
            text_field("title"),
            FieldDefinition {
                name: "names".to_string(),
                field_type: FieldType::Row,
                fields: vec![text_field("first"), text_field("last")],
                ..Default::default()
            },
        ]);
        alter_collection_table(&conn, "posts", &def2, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("first"));
        assert!(cols.contains("last"));
    }

    // ── append_default_value ──────────────────────────────────────────────

    #[test]
    fn append_default_string() {
        let mut col = "name TEXT".to_string();
        append_default_value(&mut col, &Some(serde_json::json!("hello")), &FieldType::Text);
        assert!(col.contains("DEFAULT 'hello'"));
    }

    #[test]
    fn append_default_number() {
        let mut col = "count REAL".to_string();
        append_default_value(&mut col, &Some(serde_json::json!(42)), &FieldType::Number);
        assert!(col.contains("DEFAULT 42"));
    }

    #[test]
    fn append_default_bool() {
        let mut col = "active INTEGER".to_string();
        append_default_value(&mut col, &Some(serde_json::json!(true)), &FieldType::Checkbox);
        assert!(col.contains("DEFAULT 1"));
    }

    #[test]
    fn append_default_checkbox_none() {
        let mut col = "active INTEGER".to_string();
        append_default_value(&mut col, &None, &FieldType::Checkbox);
        assert!(col.contains("DEFAULT 0"));
    }

    #[test]
    fn append_default_none_non_checkbox() {
        let mut col = "name TEXT".to_string();
        append_default_value(&mut col, &None, &FieldType::Text);
        assert!(!col.contains("DEFAULT"));
    }

    // ── collapsible fields (no prefix, same as row) ─────────────────────

    #[test]
    fn collapsible_field_promotes_sub_fields_without_prefix() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![
            FieldDefinition {
                name: "details".to_string(),
                field_type: FieldType::Collapsible,
                fields: vec![text_field("summary"), text_field("notes")],
                ..Default::default()
            },
        ]);
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("summary"), "Collapsible sub-field should be promoted");
        assert!(cols.contains("notes"), "Collapsible sub-field should be promoted");
        assert!(!cols.contains("details"), "Collapsible container should not be a column");
        assert!(!cols.contains("details__summary"), "Collapsible should not use prefix");
    }

    // ── tabs fields (no prefix, same as row) ────────────────────────────

    #[test]
    fn tabs_field_promotes_sub_fields_without_prefix() {
        use crate::core::field::FieldTab;
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![
            FieldDefinition {
                name: "layout".to_string(),
                field_type: FieldType::Tabs,
                tabs: vec![
                    FieldTab { label: "Content".to_string(), description: None, fields: vec![text_field("body")] },
                    FieldTab { label: "SEO".to_string(), description: None, fields: vec![text_field("meta_title")] },
                ],
                ..Default::default()
            },
        ]);
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("body"), "Tabs sub-field should be promoted");
        assert!(cols.contains("meta_title"), "Tabs sub-field should be promoted");
        assert!(!cols.contains("layout"), "Tabs container should not be a column");
    }

    // ── tabs containing group (the regression case) ─────────────────────

    #[test]
    fn tabs_containing_group_creates_prefixed_columns() {
        use crate::core::field::FieldTab;
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![
            FieldDefinition {
                name: "layout".to_string(),
                field_type: FieldType::Tabs,
                tabs: vec![
                    FieldTab {
                        label: "Social".to_string(),
                        description: None,
                        fields: vec![
                            FieldDefinition {
                                name: "social".to_string(),
                                field_type: FieldType::Group,
                                fields: vec![text_field("github"), text_field("twitter")],
                                ..Default::default()
                            },
                        ],
                    },
                    FieldTab {
                        label: "Content".to_string(),
                        description: None,
                        fields: vec![text_field("body")],
                    },
                ],
                ..Default::default()
            },
        ]);
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("social__github"), "Group inside Tabs should use group__subfield");
        assert!(cols.contains("social__twitter"), "Group inside Tabs should use group__subfield");
        assert!(cols.contains("body"), "Plain field in Tabs should be promoted flat");
        assert!(!cols.contains("social"), "Group itself should not be a column");
    }

    // ── collapsible containing group ────────────────────────────────────

    #[test]
    fn collapsible_containing_group_creates_prefixed_columns() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![
            FieldDefinition {
                name: "extra".to_string(),
                field_type: FieldType::Collapsible,
                fields: vec![
                    FieldDefinition {
                        name: "seo".to_string(),
                        field_type: FieldType::Group,
                        fields: vec![text_field("title"), text_field("desc")],
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        ]);
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("seo__title"), "Group inside Collapsible should use group__subfield");
        assert!(cols.contains("seo__desc"), "Group inside Collapsible should use group__subfield");
        assert!(!cols.contains("seo"), "Group itself should not be a column");
    }

    // ── alter adds tabs/collapsible sub-fields ──────────────────────────

    #[test]
    fn alter_adds_collapsible_sub_fields() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def1 = simple_collection("posts", vec![text_field("title")]);
        create_collection_table(&conn, "posts", &def1, &no_locale()).unwrap();

        let def2 = simple_collection("posts", vec![
            text_field("title"),
            FieldDefinition {
                name: "extra".to_string(),
                field_type: FieldType::Collapsible,
                fields: vec![text_field("notes")],
                ..Default::default()
            },
        ]);
        alter_collection_table(&conn, "posts", &def2, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("notes"));
    }

    #[test]
    fn alter_adds_tabs_sub_fields() {
        use crate::core::field::FieldTab;
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def1 = simple_collection("posts", vec![text_field("title")]);
        create_collection_table(&conn, "posts", &def1, &no_locale()).unwrap();

        let def2 = simple_collection("posts", vec![
            text_field("title"),
            FieldDefinition {
                name: "tabs".to_string(),
                field_type: FieldType::Tabs,
                tabs: vec![
                    FieldTab { label: "T1".to_string(), description: None, fields: vec![text_field("body")] },
                ],
                ..Default::default()
            },
        ]);
        alter_collection_table(&conn, "posts", &def2, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("body"));
    }

    #[test]
    fn alter_adds_tabs_with_group_sub_fields() {
        use crate::core::field::FieldTab;
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def1 = simple_collection("posts", vec![text_field("title")]);
        create_collection_table(&conn, "posts", &def1, &no_locale()).unwrap();

        let def2 = simple_collection("posts", vec![
            text_field("title"),
            FieldDefinition {
                name: "tabs".to_string(),
                field_type: FieldType::Tabs,
                tabs: vec![
                    FieldTab {
                        label: "SEO".to_string(),
                        description: None,
                        fields: vec![
                            FieldDefinition {
                                name: "seo".to_string(),
                                field_type: FieldType::Group,
                                fields: vec![text_field("og_title"), text_field("og_desc")],
                                ..Default::default()
                            },
                        ],
                    },
                ],
                ..Default::default()
            },
        ]);
        alter_collection_table(&conn, "posts", &def2, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("seo__og_title"), "ALTER should add Group columns inside Tabs");
        assert!(cols.contains("seo__og_desc"), "ALTER should add Group columns inside Tabs");
    }

    // ── deeply nested: tabs → collapsible → group ───────────────────────

    #[test]
    fn deeply_nested_tabs_collapsible_group() {
        use crate::core::field::FieldTab;
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![
            FieldDefinition {
                name: "layout".to_string(),
                field_type: FieldType::Tabs,
                tabs: vec![
                    FieldTab {
                        label: "Advanced".to_string(),
                        description: None,
                        fields: vec![
                            FieldDefinition {
                                name: "advanced".to_string(),
                                field_type: FieldType::Collapsible,
                                fields: vec![
                                    FieldDefinition {
                                        name: "og".to_string(),
                                        field_type: FieldType::Group,
                                        fields: vec![text_field("image"), text_field("title")],
                                        ..Default::default()
                                    },
                                    text_field("canonical"),
                                ],
                                ..Default::default()
                            },
                        ],
                    },
                ],
                ..Default::default()
            },
        ]);
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("og__image"), "Deeply nested Group inside Collapsible inside Tabs");
        assert!(cols.contains("og__title"), "Deeply nested Group inside Collapsible inside Tabs");
        assert!(cols.contains("canonical"), "Plain field in Collapsible inside Tabs");
    }

    // ── Group containing layout fields (the former terminal-node bug) ─────

    #[test]
    fn group_containing_row() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![
            FieldDefinition {
                name: "meta".to_string(),
                field_type: FieldType::Group,
                fields: vec![
                    FieldDefinition {
                        name: "row1".to_string(),
                        field_type: FieldType::Row,
                        fields: vec![text_field("title"), text_field("slug")],
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        ]);
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("meta__title"), "Group→Row should produce meta__title");
        assert!(cols.contains("meta__slug"), "Group→Row should produce meta__slug");
    }

    #[test]
    fn group_containing_collapsible() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![
            FieldDefinition {
                name: "seo".to_string(),
                field_type: FieldType::Group,
                fields: vec![
                    FieldDefinition {
                        name: "advanced".to_string(),
                        field_type: FieldType::Collapsible,
                        fields: vec![text_field("robots"), text_field("canonical")],
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        ]);
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("seo__robots"), "Group→Collapsible should produce seo__robots");
        assert!(cols.contains("seo__canonical"), "Group→Collapsible should produce seo__canonical");
    }

    #[test]
    fn group_containing_tabs() {
        use crate::core::field::FieldTab;
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![
            FieldDefinition {
                name: "settings".to_string(),
                field_type: FieldType::Group,
                fields: vec![
                    FieldDefinition {
                        name: "layout".to_string(),
                        field_type: FieldType::Tabs,
                        tabs: vec![
                            FieldTab {
                                label: "General".to_string(),
                                description: None,
                                fields: vec![text_field("theme")],
                            },
                            FieldTab {
                                label: "Advanced".to_string(),
                                description: None,
                                fields: vec![text_field("cache_ttl")],
                            },
                        ],
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        ]);
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("settings__theme"), "Group→Tabs should produce settings__theme");
        assert!(cols.contains("settings__cache_ttl"), "Group→Tabs should produce settings__cache_ttl");
    }

    #[test]
    fn group_tabs_group_three_levels() {
        use crate::core::field::FieldTab;
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![
            FieldDefinition {
                name: "outer".to_string(),
                field_type: FieldType::Group,
                fields: vec![
                    FieldDefinition {
                        name: "layout".to_string(),
                        field_type: FieldType::Tabs,
                        tabs: vec![FieldTab {
                            label: "Nested".to_string(),
                            description: None,
                            fields: vec![
                                FieldDefinition {
                                    name: "inner".to_string(),
                                    field_type: FieldType::Group,
                                    fields: vec![text_field("deep_value")],
                                    ..Default::default()
                                },
                            ],
                        }],
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        ]);
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("outer__inner__deep_value"), "Group→Tabs→Group should produce outer__inner__deep_value");
    }

    #[test]
    fn group_row_group_collapsible_four_levels() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![
            FieldDefinition {
                name: "a".to_string(),
                field_type: FieldType::Group,
                fields: vec![
                    FieldDefinition {
                        name: "r".to_string(),
                        field_type: FieldType::Row,
                        fields: vec![
                            FieldDefinition {
                                name: "b".to_string(),
                                field_type: FieldType::Group,
                                fields: vec![
                                    FieldDefinition {
                                        name: "c".to_string(),
                                        field_type: FieldType::Collapsible,
                                        fields: vec![text_field("leaf")],
                                        ..Default::default()
                                    },
                                ],
                                ..Default::default()
                            },
                        ],
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        ]);
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("a__b__leaf"), "Group→Row→Group→Collapsible: a__b__leaf");
    }

    #[test]
    fn group_containing_tabs_with_locale() {
        use crate::core::field::FieldTab;
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![
            FieldDefinition {
                name: "meta".to_string(),
                field_type: FieldType::Group,
                localized: true,
                fields: vec![
                    FieldDefinition {
                        name: "layout".to_string(),
                        field_type: FieldType::Tabs,
                        tabs: vec![FieldTab {
                            label: "Content".to_string(),
                            description: None,
                            fields: vec![text_field("title")],
                        }],
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        ]);
        create_collection_table(&conn, "posts", &def, &locale_en_de()).unwrap();
        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("meta__title__en"), "Localized Group→Tabs: meta__title__en");
        assert!(cols.contains("meta__title__de"), "Localized Group→Tabs: meta__title__de");
    }

    // ── sync_indexes tests ──────────────────────────────────────────────

    fn get_indexes(conn: &rusqlite::Connection, table: &str) -> HashSet<String> {
        let mut stmt = conn.prepare(
            "SELECT name FROM sqlite_master WHERE type='index' AND tbl_name=?1"
        ).unwrap();
        stmt.query_map([table], |row| row.get::<_, String>(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect()
    }

    #[test]
    fn sync_indexes_creates_index_for_indexed_field() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![
            FieldDefinition {
                name: "status".to_string(),
                index: true,
                ..Default::default()
            },
        ]);
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        sync_indexes(&conn, "posts", &def, &no_locale()).unwrap();

        let indexes = get_indexes(&conn, "posts");
        assert!(indexes.contains("idx_posts_status"), "Should create index for index=true field");
    }

    #[test]
    fn sync_indexes_skips_unique_field() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![
            FieldDefinition {
                name: "slug".to_string(),
                unique: true,
                index: true, // should be skipped because unique=true
                ..Default::default()
            },
        ]);
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        sync_indexes(&conn, "posts", &def, &no_locale()).unwrap();

        let indexes = get_indexes(&conn, "posts");
        assert!(!indexes.contains("idx_posts_slug"), "Should skip index when unique=true");
    }

    #[test]
    fn sync_indexes_creates_compound_index() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let mut def = simple_collection("posts", vec![
            text_field("status"),
            text_field("category"),
        ]);
        def.indexes = vec![IndexDefinition {
            fields: vec!["status".to_string(), "category".to_string()],
            unique: false,
        }];
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        sync_indexes(&conn, "posts", &def, &no_locale()).unwrap();

        let indexes = get_indexes(&conn, "posts");
        assert!(indexes.contains("idx_posts_status_category"), "Should create compound index");
    }

    #[test]
    fn sync_indexes_creates_compound_unique_index() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let mut def = simple_collection("posts", vec![
            text_field("category"),
            text_field("slug"),
        ]);
        def.indexes = vec![IndexDefinition {
            fields: vec!["category".to_string(), "slug".to_string()],
            unique: true,
        }];
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        sync_indexes(&conn, "posts", &def, &no_locale()).unwrap();

        let indexes = get_indexes(&conn, "posts");
        assert!(indexes.contains("idx_posts_category_slug"), "Should create compound unique index");
    }

    #[test]
    fn sync_indexes_drops_stale_indexes() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let mut def = simple_collection("posts", vec![
            text_field("status"),
            text_field("category"),
        ]);
        def.indexes = vec![IndexDefinition {
            fields: vec!["status".to_string()],
            unique: false,
        }];
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        sync_indexes(&conn, "posts", &def, &no_locale()).unwrap();
        assert!(get_indexes(&conn, "posts").contains("idx_posts_status"));

        // Remove the compound index, add a different one
        def.indexes = vec![IndexDefinition {
            fields: vec!["category".to_string()],
            unique: false,
        }];
        sync_indexes(&conn, "posts", &def, &no_locale()).unwrap();

        let indexes = get_indexes(&conn, "posts");
        assert!(!indexes.contains("idx_posts_status"), "Old index should be dropped");
        assert!(indexes.contains("idx_posts_category"), "New index should be created");
    }

    #[test]
    fn sync_indexes_localized_field() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![
            FieldDefinition {
                name: "title".to_string(),
                localized: true,
                index: true,
                ..Default::default()
            },
        ]);
        create_collection_table(&conn, "posts", &def, &locale_en_de()).unwrap();
        sync_indexes(&conn, "posts", &def, &locale_en_de()).unwrap();

        let indexes = get_indexes(&conn, "posts");
        assert!(indexes.contains("idx_posts_title__en"), "Should create index per locale: en");
        assert!(indexes.contains("idx_posts_title__de"), "Should create index per locale: de");
    }

    #[test]
    fn sync_indexes_idempotent() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![
            FieldDefinition {
                name: "status".to_string(),
                index: true,
                ..Default::default()
            },
        ]);
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();

        // Run twice — should not error
        sync_indexes(&conn, "posts", &def, &no_locale()).unwrap();
        sync_indexes(&conn, "posts", &def, &no_locale()).unwrap();

        let indexes = get_indexes(&conn, "posts");
        assert!(indexes.contains("idx_posts_status"));
    }

    #[test]
    fn sync_indexes_validates_compound_field_names() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let mut def = simple_collection("posts", vec![text_field("title")]);
        def.indexes = vec![IndexDefinition {
            fields: vec!["1=1; DROP TABLE posts; --".to_string()],
            unique: false,
        }];
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();

        let result = sync_indexes(&conn, "posts", &def, &no_locale());
        assert!(result.is_err(), "Should reject invalid identifier in compound index");
    }
}
