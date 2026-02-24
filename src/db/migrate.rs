//! Dynamic schema migration: syncs SQLite tables to match Lua collection definitions.

use anyhow::{Context, Result};
use std::collections::HashSet;

use crate::config::LocaleConfig;
use crate::core::SharedRegistry;
use crate::core::field::FieldType;
use super::DbPool;

/// Sync all collection tables with their Lua definitions.
pub fn sync_all(pool: &DbPool, registry: &SharedRegistry, locale_config: &LocaleConfig) -> Result<()> {
    let mut conn = pool.get().context("Failed to get DB connection")?;
    let tx = conn.transaction().context("Failed to start migration transaction")?;

    // Create metadata table
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS _crap_meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL,
            updated_at TEXT DEFAULT (datetime('now'))
        );"
    ).context("Failed to create _crap_meta table")?;

    // Create migrations tracking table
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS _crap_migrations (
            filename TEXT PRIMARY KEY,
            applied_at TEXT DEFAULT (datetime('now'))
        );"
    ).context("Failed to create _crap_migrations table")?;

    let reg = registry.read()
        .map_err(|e| anyhow::anyhow!("Registry lock poisoned: {}", e))?;

    for (slug, def) in &reg.collections {
        sync_collection_table(&tx, slug, def, locale_config)?;
    }

    for (slug, def) in &reg.globals {
        sync_global_table(&tx, slug, def, locale_config)?;
    }

    drop(reg);
    tx.commit().context("Failed to commit migration transaction")?;

    Ok(())
}

fn sync_collection_table(
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

    Ok(())
}

fn sync_global_table(
    conn: &rusqlite::Connection,
    slug: &str,
    def: &crate::core::collection::GlobalDefinition,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let table_name = format!("_global_{}", slug);
    let table_exists = table_exists(conn, &table_name)?;

    if !table_exists {
        let mut columns = vec!["id TEXT PRIMARY KEY".to_string()];

        for field in &def.fields {
            if field.localized && locale_config.is_enabled() {
                // Localized fields get one column per locale
                for locale in &locale_config.locales {
                    let col = format!("{}__{} {}", field.name, locale, field.field_type.sqlite_type());
                    columns.push(col);
                }
            } else {
                let col = format!("{} {}", field.name, field.field_type.sqlite_type());
                columns.push(col);
            }
        }

        columns.push("created_at TEXT DEFAULT (datetime('now'))".to_string());
        columns.push("updated_at TEXT DEFAULT (datetime('now'))".to_string());

        let sql = format!(
            "CREATE TABLE {} ({})",
            table_name,
            columns.join(", ")
        );

        tracing::info!("Creating global table: {}", table_name);
        conn.execute(&sql, [])
            .with_context(|| format!("Failed to create table {}", table_name))?;

        // Insert the single global row
        conn.execute(
            &format!("INSERT OR IGNORE INTO {} (id) VALUES ('default')", table_name),
            [],
        )?;
    }

    Ok(())
}

fn table_exists(conn: &rusqlite::Connection, name: &str) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
        [name],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

fn create_collection_table(
    conn: &rusqlite::Connection,
    slug: &str,
    def: &crate::core::CollectionDefinition,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let mut columns = vec!["id TEXT PRIMARY KEY".to_string()];

    for field in &def.fields {
        // Group fields expand sub-fields as prefixed columns
        if field.field_type == FieldType::Group {
            for sub in &field.fields {
                let base_col_name = format!("{}__{}", field.name, sub.name);
                let is_localized = (field.localized || sub.localized) && locale_config.is_enabled();

                if is_localized {
                    for locale in &locale_config.locales {
                        let col_name = format!("{}__{}", base_col_name, locale);
                        let mut col = format!("{} {}", col_name, sub.field_type.sqlite_type());
                        // Required only on default locale (skip NOT NULL for drafts — app-level validation on publish)
                        if sub.required && *locale == locale_config.default_locale && !def.has_drafts() {
                            col.push_str(" NOT NULL");
                        }
                        if sub.unique {
                            col.push_str(" UNIQUE");
                        }
                        append_default_value(&mut col, &sub.default_value, &sub.field_type);
                        columns.push(col);
                    }
                } else {
                    let mut col = format!("{} {}", base_col_name, sub.field_type.sqlite_type());
                    if sub.required && !def.has_drafts() {
                        col.push_str(" NOT NULL");
                    }
                    if sub.unique {
                        col.push_str(" UNIQUE");
                    }
                    append_default_value(&mut col, &sub.default_value, &sub.field_type);
                    columns.push(col);
                }
            }
            continue;
        }
        // Skip fields that use join tables (array, blocks, has-many relationship)
        if !field.has_parent_column() {
            continue;
        }

        if field.localized && locale_config.is_enabled() {
            // Localized fields get one column per locale
            for locale in &locale_config.locales {
                let col_name = format!("{}__{}", field.name, locale);
                let mut col = format!("{} {}", col_name, field.field_type.sqlite_type());
                // Required only on default locale (skip NOT NULL for drafts — app-level validation on publish)
                if field.required && *locale == locale_config.default_locale && !def.has_drafts() {
                    col.push_str(" NOT NULL");
                }
                if field.unique {
                    col.push_str(" UNIQUE");
                }
                append_default_value(&mut col, &field.default_value, &field.field_type);
                columns.push(col);
            }
        } else {
            let mut col = format!("{} {}", field.name, field.field_type.sqlite_type());
            if field.required && !def.has_drafts() {
                col.push_str(" NOT NULL");
            }
            if field.unique {
                col.push_str(" UNIQUE");
            }
            append_default_value(&mut col, &field.default_value, &field.field_type);
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
        if def.auth.as_ref().is_some_and(|a| a.verify_email) {
            columns.push("_verified INTEGER DEFAULT 0".to_string());
            columns.push("_verification_token TEXT".to_string());
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

    for field in &def.fields {
        // Group fields expand sub-fields as prefixed columns
        if field.field_type == FieldType::Group {
            for sub in &field.fields {
                let base_col_name = format!("{}__{}", field.name, sub.name);
                let is_localized = (field.localized || sub.localized) && locale_config.is_enabled();

                if is_localized {
                    for locale in &locale_config.locales {
                        let col_name = format!("{}__{}", base_col_name, locale);
                        if !existing_columns.contains(&col_name) {
                            let mut col_def = sub.field_type.sqlite_type().to_string();
                            append_default_value(&mut col_def, &sub.default_value, &sub.field_type);
                            let sql = format!("ALTER TABLE {} ADD COLUMN {} {}", slug, col_name, col_def);
                            tracing::info!("Adding column to {}: {}", slug, col_name);
                            conn.execute(&sql, [])
                                .with_context(|| format!("Failed to add column {} to {}", col_name, slug))?;
                        }
                    }
                } else if !existing_columns.contains(&base_col_name) {
                    let mut col_def = sub.field_type.sqlite_type().to_string();
                    append_default_value(&mut col_def, &sub.default_value, &sub.field_type);
                    let sql = format!("ALTER TABLE {} ADD COLUMN {} {}", slug, base_col_name, col_def);
                    tracing::info!("Adding column to {}: {}", slug, base_col_name);
                    conn.execute(&sql, [])
                        .with_context(|| format!("Failed to add column {} to {}", base_col_name, slug))?;
                }
            }
            continue;
        }
        // Skip fields that use join tables (array, blocks, has-many relationship)
        if !field.has_parent_column() {
            continue;
        }

        if field.localized && locale_config.is_enabled() {
            for locale in &locale_config.locales {
                let col_name = format!("{}__{}", field.name, locale);
                if !existing_columns.contains(&col_name) {
                    let mut col_def = field.field_type.sqlite_type().to_string();
                    append_default_value(&mut col_def, &field.default_value, &field.field_type);
                    let sql = format!("ALTER TABLE {} ADD COLUMN {} {}", slug, col_name, col_def);
                    tracing::info!("Adding column to {}: {}", slug, col_name);
                    conn.execute(&sql, [])
                        .with_context(|| format!("Failed to add column {} to {}", col_name, slug))?;
                }
            }
        } else if !existing_columns.contains(&field.name) {
            let mut col_def = field.field_type.sqlite_type().to_string();
            append_default_value(&mut col_def, &field.default_value, &field.field_type);
            let sql = format!("ALTER TABLE {} ADD COLUMN {} {}", slug, field.name, col_def);
            tracing::info!("Adding column to {}: {}", slug, field.name);
            conn.execute(&sql, [])
                .with_context(|| format!("Failed to add column {} to {}", field.name, slug))?;
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
        for col in ["_password_hash TEXT", "_reset_token TEXT", "_reset_token_exp INTEGER", "_locked INTEGER DEFAULT 0"] {
            let col_name = col.split_whitespace().next().unwrap();
            if !existing_columns.contains(col_name) {
                let sql = format!("ALTER TABLE {} ADD COLUMN {}", slug, col);
                tracing::info!("Adding {} column to {}", col_name, slug);
                conn.execute(&sql, [])
                    .with_context(|| format!("Failed to add {} to {}", col_name, slug))?;
            }
        }
        if def.auth.as_ref().is_some_and(|a| a.verify_email) {
            for col in ["_verified INTEGER DEFAULT 0", "_verification_token TEXT"] {
                let col_name = col.split_whitespace().next().unwrap();
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
        "_locked", "_status",
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

/// Sync join tables for has-many relationships and array fields.
fn sync_join_tables(
    conn: &rusqlite::Connection,
    collection_slug: &str,
    fields: &[crate::core::field::FieldDefinition],
    locale_config: &LocaleConfig,
) -> Result<()> {
    use crate::core::field::FieldType;

    for field in fields {
        let has_locale_col = field.localized && locale_config.is_enabled();

        match field.field_type {
            FieldType::Relationship => {
                if let Some(ref rc) = field.relationship {
                    if rc.has_many {
                        let table_name = format!("{}_{}", collection_slug, field.name);
                        if !table_exists(conn, &table_name)? {
                            let sql = if has_locale_col {
                                format!(
                                    "CREATE TABLE {} (\
                                        parent_id TEXT NOT NULL REFERENCES {}(id) ON DELETE CASCADE, \
                                        related_id TEXT NOT NULL, \
                                        _order INTEGER NOT NULL DEFAULT 0, \
                                        _locale TEXT NOT NULL DEFAULT '{}', \
                                        PRIMARY KEY (parent_id, related_id, _locale)\
                                    )",
                                    table_name, collection_slug, locale_config.default_locale
                                )
                            } else {
                                format!(
                                    "CREATE TABLE {} (\
                                        parent_id TEXT NOT NULL REFERENCES {}(id) ON DELETE CASCADE, \
                                        related_id TEXT NOT NULL, \
                                        _order INTEGER NOT NULL DEFAULT 0, \
                                        PRIMARY KEY (parent_id, related_id)\
                                    )",
                                    table_name, collection_slug
                                )
                            };
                            tracing::info!("Creating junction table: {}", table_name);
                            conn.execute(&sql, [])
                                .with_context(|| format!("Failed to create junction table {}", table_name))?;
                        } else if has_locale_col {
                            ensure_locale_column(conn, &table_name, &locale_config.default_locale)?;
                        }
                    }
                }
            }
            FieldType::Array => {
                let table_name = format!("{}_{}", collection_slug, field.name);
                if !table_exists(conn, &table_name)? {
                    let mut columns = vec![
                        "id TEXT PRIMARY KEY".to_string(),
                        format!("parent_id TEXT NOT NULL REFERENCES {}(id) ON DELETE CASCADE", collection_slug),
                        "_order INTEGER NOT NULL DEFAULT 0".to_string(),
                    ];
                    if has_locale_col {
                        columns.push(format!("_locale TEXT NOT NULL DEFAULT '{}'", locale_config.default_locale));
                    }
                    for sub_field in &field.fields {
                        columns.push(format!("{} {}", sub_field.name, sub_field.field_type.sqlite_type()));
                    }
                    let sql = format!(
                        "CREATE TABLE {} ({})",
                        table_name,
                        columns.join(", ")
                    );
                    tracing::info!("Creating array table: {}", table_name);
                    conn.execute(&sql, [])
                        .with_context(|| format!("Failed to create array table {}", table_name))?;
                } else {
                    if has_locale_col {
                        ensure_locale_column(conn, &table_name, &locale_config.default_locale)?;
                    }
                    // Alter: add missing sub-field columns
                    let existing = get_table_columns(conn, &table_name)?;
                    for sub_field in &field.fields {
                        if !existing.contains(&sub_field.name) {
                            let sql = format!(
                                "ALTER TABLE {} ADD COLUMN {} {}",
                                table_name, sub_field.name, sub_field.field_type.sqlite_type()
                            );
                            tracing::info!("Adding column to {}: {}", table_name, sub_field.name);
                            conn.execute(&sql, [])
                                .with_context(|| format!("Failed to add column {} to {}", sub_field.name, table_name))?;
                        }
                    }
                }
            }
            FieldType::Blocks => {
                let table_name = format!("{}_{}", collection_slug, field.name);
                if !table_exists(conn, &table_name)? {
                    let locale_col = if has_locale_col {
                        format!(", _locale TEXT NOT NULL DEFAULT '{}'", locale_config.default_locale)
                    } else {
                        String::new()
                    };
                    let sql = format!(
                        "CREATE TABLE {} (\
                            id TEXT PRIMARY KEY, \
                            parent_id TEXT NOT NULL REFERENCES {}(id) ON DELETE CASCADE, \
                            _order INTEGER NOT NULL DEFAULT 0, \
                            _block_type TEXT NOT NULL, \
                            data TEXT NOT NULL DEFAULT '{{}}'\
                            {}\
                        )",
                        table_name, collection_slug, locale_col
                    );
                    tracing::info!("Creating blocks table: {}", table_name);
                    conn.execute(&sql, [])
                        .with_context(|| format!("Failed to create blocks table {}", table_name))?;
                } else if has_locale_col {
                    ensure_locale_column(conn, &table_name, &locale_config.default_locale)?;
                }
            }
            _ => {}
        }
    }

    Ok(())
}

/// Create or verify the `_versions_{slug}` table for document version history.
fn sync_versions_table(conn: &rusqlite::Connection, slug: &str) -> Result<()> {
    let table_name = format!("_versions_{}", slug);
    if !table_exists(conn, &table_name)? {
        let sql = format!(
            "CREATE TABLE {} (\
                id TEXT PRIMARY KEY, \
                _parent TEXT NOT NULL REFERENCES {}(id) ON DELETE CASCADE, \
                _version INTEGER NOT NULL, \
                _status TEXT NOT NULL, \
                _latest INTEGER NOT NULL DEFAULT 0, \
                snapshot TEXT NOT NULL, \
                created_at TEXT DEFAULT (datetime('now')), \
                updated_at TEXT DEFAULT (datetime('now'))\
            )",
            table_name, slug
        );
        tracing::info!("Creating versions table: {}", table_name);
        conn.execute(&sql, [])
            .with_context(|| format!("Failed to create versions table {}", table_name))?;

        // Indexes for efficient version lookups
        conn.execute(
            &format!(
                "CREATE INDEX IF NOT EXISTS idx_{slug}_parent_latest ON {table} (_parent, _latest)",
                slug = slug, table = table_name
            ),
            [],
        )?;
        conn.execute(
            &format!(
                "CREATE INDEX IF NOT EXISTS idx_{slug}_parent_version ON {table} (_parent, _version DESC)",
                slug = slug, table = table_name
            ),
            [],
        )?;
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

/// Ensure a `_locale` column exists on a junction table (for ALTER TABLE on existing tables).
fn ensure_locale_column(conn: &rusqlite::Connection, table_name: &str, default_locale: &str) -> Result<()> {
    let existing = get_table_columns(conn, table_name)?;
    if !existing.contains("_locale") {
        let sql = format!(
            "ALTER TABLE {} ADD COLUMN _locale TEXT NOT NULL DEFAULT '{}'",
            table_name, default_locale
        );
        tracing::info!("Adding _locale column to {}", table_name);
        conn.execute(&sql, [])
            .with_context(|| format!("Failed to add _locale to {}", table_name))?;
    }
    Ok(())
}

fn get_table_columns(conn: &rusqlite::Connection, table: &str) -> Result<HashSet<String>> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({})", table))?;
    let columns: HashSet<String> = stmt.query_map([], |row| {
        row.get::<_, String>(1)
    })?.filter_map(|r| r.ok()).collect();
    Ok(columns)
}

// ---------------------------------------------------------------------------
// Migration tracking
// ---------------------------------------------------------------------------

/// List all `*.lua` files in the migrations directory, sorted by filename (chronological).
pub fn list_migration_files(migrations_dir: &std::path::Path) -> Result<Vec<String>> {
    if !migrations_dir.exists() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    for entry in std::fs::read_dir(migrations_dir)
        .with_context(|| format!("Failed to read {}", migrations_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "lua") {
            if let Some(name) = path.file_name() {
                files.push(name.to_string_lossy().to_string());
            }
        }
    }
    files.sort();
    Ok(files)
}

/// Get filenames of all applied migrations (unordered set).
pub fn get_applied_migrations(pool: &DbPool) -> Result<HashSet<String>> {
    let conn = pool.get().context("Failed to get DB connection")?;
    // Table may not exist yet if sync_all hasn't run
    let exists = table_exists(&conn, "_crap_migrations")?;
    if !exists {
        return Ok(HashSet::new());
    }
    let mut stmt = conn.prepare("SELECT filename FROM _crap_migrations")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    let mut set = HashSet::new();
    for r in rows {
        set.insert(r?);
    }
    Ok(set)
}

/// Get applied migration filenames, most recent first.
pub fn get_applied_migrations_desc(pool: &DbPool) -> Result<Vec<String>> {
    let conn = pool.get().context("Failed to get DB connection")?;
    let exists = table_exists(&conn, "_crap_migrations")?;
    if !exists {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare("SELECT filename FROM _crap_migrations ORDER BY applied_at DESC, filename DESC")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    let mut list = Vec::new();
    for r in rows {
        list.push(r?);
    }
    Ok(list)
}

/// Get pending migration filenames (files on disk minus already applied), sorted ascending.
pub fn get_pending_migrations(pool: &DbPool, migrations_dir: &std::path::Path) -> Result<Vec<String>> {
    let all = list_migration_files(migrations_dir)?;
    let applied = get_applied_migrations(pool)?;
    Ok(all.into_iter().filter(|f| !applied.contains(f)).collect())
}

/// Record a migration as applied.
pub fn record_migration(conn: &rusqlite::Connection, filename: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO _crap_migrations (filename) VALUES (?1)",
        [filename],
    ).with_context(|| format!("Failed to record migration {}", filename))?;
    Ok(())
}

/// Remove a migration record (for rollback).
pub fn remove_migration(conn: &rusqlite::Connection, filename: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM _crap_migrations WHERE filename = ?1",
        [filename],
    ).with_context(|| format!("Failed to remove migration record {}", filename))?;
    Ok(())
}

/// Drop all user tables (for `migrate fresh`). Drops everything except sqlite internals.
pub fn drop_all_tables(pool: &DbPool) -> Result<()> {
    let conn = pool.get().context("Failed to get DB connection")?;
    let mut stmt = conn.prepare(
        "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'"
    )?;
    let tables: Vec<String> = stmt.query_map([], |row| row.get::<_, String>(0))?
        .filter_map(|r| r.ok())
        .collect();
    drop(stmt);

    for table in &tables {
        conn.execute(&format!("DROP TABLE IF EXISTS \"{}\"", table), [])
            .with_context(|| format!("Failed to drop table {}", table))?;
        tracing::info!("Dropped table: {}", table);
    }
    Ok(())
}
