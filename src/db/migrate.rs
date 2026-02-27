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

    // Create jobs table
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS _crap_jobs (
            id TEXT PRIMARY KEY,
            slug TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT 'pending',
            queue TEXT NOT NULL DEFAULT 'default',
            data TEXT DEFAULT '{}',
            result TEXT,
            error TEXT,
            attempt INTEGER NOT NULL DEFAULT 0,
            max_attempts INTEGER NOT NULL DEFAULT 1,
            scheduled_by TEXT,
            created_at TEXT DEFAULT (datetime('now')),
            started_at TEXT,
            completed_at TEXT,
            heartbeat_at TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_crap_jobs_status ON _crap_jobs(status);
        CREATE INDEX IF NOT EXISTS idx_crap_jobs_queue ON _crap_jobs(queue, status);
        CREATE INDEX IF NOT EXISTS idx_crap_jobs_slug ON _crap_jobs(slug, status);"
    ).context("Failed to create _crap_jobs table")?;

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
    let exists = table_exists(conn, &table_name)?;

    if !exists {
        let mut columns = vec!["id TEXT PRIMARY KEY".to_string()];

        for field in &def.fields {
            // Group fields expand sub-fields as prefixed columns (same as collections)
            if field.field_type == FieldType::Group {
                for sub in &field.fields {
                    let base_col_name = format!("{}__{}", field.name, sub.name);
                    let is_localized = (field.localized || sub.localized) && locale_config.is_enabled();

                    if is_localized {
                        for locale in &locale_config.locales {
                            let col = format!("{}__{} {}", base_col_name, locale, sub.field_type.sqlite_type());
                            columns.push(col);
                        }
                    } else {
                        let col = format!("{} {}", base_col_name, sub.field_type.sqlite_type());
                        columns.push(col);
                    }
                }
                continue;
            }
            // Skip fields that use join tables (array, blocks, has-many relationship).
            if !field.has_parent_column() {
                continue;
            }

            if field.localized && locale_config.is_enabled() {
                for locale in &locale_config.locales {
                    let col = format!("{}__{} {}", field.name, locale, field.field_type.sqlite_type());
                    columns.push(col);
                }
            } else {
                let col = format!("{} {}", field.name, field.field_type.sqlite_type());
                columns.push(col);
            }
        }

        // Versioned globals with drafts get a _status column
        if def.has_drafts() {
            columns.push("_status TEXT NOT NULL DEFAULT 'published'".to_string());
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
    } else {
        // ALTER TABLE: add columns for new scalar/group fields
        let existing_columns = get_table_columns(conn, &table_name)?;

        for field in &def.fields {
            // Group fields expand sub-fields as prefixed columns (same as collections)
            if field.field_type == FieldType::Group {
                for sub in &field.fields {
                    let base_col_name = format!("{}__{}", field.name, sub.name);
                    let is_localized = (field.localized || sub.localized) && locale_config.is_enabled();

                    if is_localized {
                        for locale in &locale_config.locales {
                            let col_name = format!("{}__{}", base_col_name, locale);
                            if !existing_columns.contains(&col_name) {
                                let sql = format!(
                                    "ALTER TABLE {} ADD COLUMN {} {}",
                                    table_name, col_name, sub.field_type.sqlite_type()
                                );
                                tracing::info!("Adding column to {}: {}", table_name, col_name);
                                conn.execute(&sql, [])
                                    .with_context(|| format!("Failed to add column {} to {}", col_name, table_name))?;
                            }
                        }
                    } else if !existing_columns.contains(&base_col_name) {
                        let sql = format!(
                            "ALTER TABLE {} ADD COLUMN {} {}",
                            table_name, base_col_name, sub.field_type.sqlite_type()
                        );
                        tracing::info!("Adding column to {}: {}", table_name, base_col_name);
                        conn.execute(&sql, [])
                            .with_context(|| format!("Failed to add column {} to {}", base_col_name, table_name))?;
                    }
                }
                continue;
            }
            // Skip fields that use join tables (array, blocks, has-many relationship).
            if !field.has_parent_column() {
                continue;
            }

            if field.localized && locale_config.is_enabled() {
                for locale in &locale_config.locales {
                    let col_name = format!("{}__{}", field.name, locale);
                    if !existing_columns.contains(&col_name) {
                        let sql = format!(
                            "ALTER TABLE {} ADD COLUMN {} {}",
                            table_name, col_name, field.field_type.sqlite_type()
                        );
                        tracing::info!("Adding column to {}: {}", table_name, col_name);
                        conn.execute(&sql, [])
                            .with_context(|| format!("Failed to add column {} to {}", col_name, table_name))?;
                    }
                }
            } else if !existing_columns.contains(&field.name) {
                let sql = format!(
                    "ALTER TABLE {} ADD COLUMN {} {}",
                    table_name, field.name, field.field_type.sqlite_type()
                );
                tracing::info!("Adding column to {}: {}", table_name, field.name);
                conn.execute(&sql, [])
                    .with_context(|| format!("Failed to add column {} to {}", field.name, table_name))?;
            }
        }
    }

    // Versioned globals with drafts: ensure _status column exists (ALTER path)
    if exists && def.has_drafts() {
        let existing_columns = get_table_columns(conn, &table_name)?;
        if !existing_columns.contains("_status") {
            let sql = format!("ALTER TABLE {} ADD COLUMN _status TEXT NOT NULL DEFAULT 'published'", table_name);
            tracing::info!("Adding _status column to {}", table_name);
            conn.execute(&sql, [])
                .with_context(|| format!("Failed to add _status to {}", table_name))?;
        }
    }

    // Sync join tables for array, blocks, and has-many relationship fields
    sync_join_tables(conn, &table_name, &def.fields, locale_config)?;

    // Sync versions table if versioning is enabled
    if def.has_versions() {
        sync_versions_table(conn, &table_name)?;
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
            for col in ["_verified INTEGER DEFAULT 0", "_verification_token TEXT", "_verification_token_exp INTEGER"] {
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
        "_verification_token_exp", "_locked", "_status",
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LocaleConfig;
    use crate::core::collection::*;
    use crate::core::field::{FieldDefinition, FieldType, RelationshipConfig};

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
            live: None,
            versions: None,
        }
    }

    fn simple_global(slug: &str, fields: Vec<FieldDefinition>) -> GlobalDefinition {
        GlobalDefinition {
            slug: slug.to_string(),
            labels: CollectionLabels::default(),
            fields,
            hooks: CollectionHooks::default(),
            access: CollectionAccess::default(),
            live: None,
            versions: None,
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

    // ── table_exists ──────────────────────────────────────────────────────

    #[test]
    fn table_exists_false_initially() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        assert!(!table_exists(&conn, "nonexistent").unwrap());
    }

    #[test]
    fn table_exists_true_after_create() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        conn.execute("CREATE TABLE test_table (id TEXT PRIMARY KEY)", []).unwrap();
        assert!(table_exists(&conn, "test_table").unwrap());
    }

    // ── get_table_columns ─────────────────────────────────────────────────

    #[test]
    fn get_table_columns_returns_column_names() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        conn.execute("CREATE TABLE t (id TEXT, name TEXT, age INTEGER)", []).unwrap();
        let cols = get_table_columns(&conn, "t").unwrap();
        assert!(cols.contains("id"));
        assert!(cols.contains("name"));
        assert!(cols.contains("age"));
        assert_eq!(cols.len(), 3);
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

    // ── join tables ───────────────────────────────────────────────────────

    #[test]
    fn has_many_relationship_creates_junction_table() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![
            FieldDefinition {
                name: "tags".to_string(),
                field_type: FieldType::Relationship,
                relationship: Some(RelationshipConfig {
                    collection: "tags".to_string(),
                    has_many: true,
                    max_depth: None,
                }),
                ..Default::default()
            },
        ]);
        // Need parent table first for FK
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &no_locale()).unwrap();

        assert!(table_exists(&conn, "posts_tags").unwrap());
        let cols = get_table_columns(&conn, "posts_tags").unwrap();
        assert!(cols.contains("parent_id"));
        assert!(cols.contains("related_id"));
        assert!(cols.contains("_order"));
    }

    #[test]
    fn array_field_creates_join_table() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![
            FieldDefinition {
                name: "items".to_string(),
                field_type: FieldType::Array,
                fields: vec![text_field("name")],
                ..Default::default()
            },
        ]);
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &no_locale()).unwrap();

        assert!(table_exists(&conn, "posts_items").unwrap());
        let cols = get_table_columns(&conn, "posts_items").unwrap();
        assert!(cols.contains("id"));
        assert!(cols.contains("parent_id"));
        assert!(cols.contains("_order"));
        assert!(cols.contains("name"));
    }

    #[test]
    fn blocks_field_creates_join_table() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![
            FieldDefinition {
                name: "content".to_string(),
                field_type: FieldType::Blocks,
                ..Default::default()
            },
        ]);
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &no_locale()).unwrap();

        assert!(table_exists(&conn, "posts_content").unwrap());
        let cols = get_table_columns(&conn, "posts_content").unwrap();
        assert!(cols.contains("id"));
        assert!(cols.contains("parent_id"));
        assert!(cols.contains("_block_type"));
        assert!(cols.contains("data"));
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

    // ── global table ──────────────────────────────────────────────────────

    #[test]
    fn global_table_created_with_default_row() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_global("settings", vec![text_field("site_name")]);
        sync_global_table(&conn, "settings", &def, &no_locale()).unwrap();

        assert!(table_exists(&conn, "_global_settings").unwrap());
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM _global_settings", [], |r| r.get(0)
        ).unwrap();
        assert_eq!(count, 1, "should have exactly one default row");
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

    // ── migration tracking ────────────────────────────────────────────────

    #[test]
    fn migration_tracking_roundtrip() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        conn.execute_batch(
            "CREATE TABLE _crap_migrations (filename TEXT PRIMARY KEY, applied_at TEXT DEFAULT (datetime('now')))"
        ).unwrap();

        record_migration(&conn, "001_init.lua").unwrap();
        record_migration(&conn, "002_add_field.lua").unwrap();

        let applied = get_applied_migrations(&pool).unwrap();
        assert!(applied.contains("001_init.lua"));
        assert!(applied.contains("002_add_field.lua"));
        assert_eq!(applied.len(), 2);
    }

    #[test]
    fn remove_migration_works() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        conn.execute_batch(
            "CREATE TABLE _crap_migrations (filename TEXT PRIMARY KEY, applied_at TEXT DEFAULT (datetime('now')))"
        ).unwrap();

        record_migration(&conn, "001_init.lua").unwrap();
        remove_migration(&conn, "001_init.lua").unwrap();

        let applied = get_applied_migrations(&pool).unwrap();
        assert!(applied.is_empty());
    }

    #[test]
    fn get_applied_migrations_no_table() {
        let pool = in_memory_pool();
        let applied = get_applied_migrations(&pool).unwrap();
        assert!(applied.is_empty());
    }

    #[test]
    fn get_pending_migrations_filters_applied() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        conn.execute_batch(
            "CREATE TABLE _crap_migrations (filename TEXT PRIMARY KEY, applied_at TEXT DEFAULT (datetime('now')))"
        ).unwrap();
        record_migration(&conn, "001_init.lua").unwrap();

        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("001_init.lua"), "-- already applied").unwrap();
        std::fs::write(tmp.path().join("002_new.lua"), "-- pending").unwrap();

        let pending = get_pending_migrations(&pool, tmp.path()).unwrap();
        assert_eq!(pending, vec!["002_new.lua"]);
    }

    #[test]
    fn list_migration_files_sorted() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("003_z.lua"), "").unwrap();
        std::fs::write(tmp.path().join("001_a.lua"), "").unwrap();
        std::fs::write(tmp.path().join("002_b.lua"), "").unwrap();
        std::fs::write(tmp.path().join("readme.txt"), "").unwrap(); // non-lua

        let files = list_migration_files(tmp.path()).unwrap();
        assert_eq!(files, vec!["001_a.lua", "002_b.lua", "003_z.lua"]);
    }

    #[test]
    fn list_migration_files_missing_dir() {
        let files = list_migration_files(std::path::Path::new("/nonexistent/dir")).unwrap();
        assert!(files.is_empty());
    }

    // ── drop_all_tables ───────────────────────────────────────────────────

    #[test]
    fn drop_all_tables_cleans_everything() {
        let pool = in_memory_pool();
        {
            let conn = pool.get().unwrap();
            conn.execute("CREATE TABLE posts (id TEXT PRIMARY KEY)", []).unwrap();
            conn.execute("CREATE TABLE users (id TEXT PRIMARY KEY)", []).unwrap();
        }
        drop_all_tables(&pool).unwrap();
        let conn = pool.get().unwrap();
        assert!(!table_exists(&conn, "posts").unwrap());
        assert!(!table_exists(&conn, "users").unwrap());
    }

    // ── global table alter (add new field to existing global) ─────────────

    #[test]
    fn global_table_alter_adds_new_column() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def1 = simple_global("settings", vec![text_field("site_name")]);
        sync_global_table(&conn, "settings", &def1, &no_locale()).unwrap();

        // Now add a new field
        let def2 = simple_global("settings", vec![
            text_field("site_name"),
            text_field("site_url"),
        ]);
        sync_global_table(&conn, "settings", &def2, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "_global_settings").unwrap();
        assert!(cols.contains("site_url"), "New column should be added via ALTER");
    }

    // ── global table with localized fields ──────────────────────────────

    #[test]
    fn global_table_localized_fields() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_global("settings", vec![localized_field("site_name")]);
        sync_global_table(&conn, "settings", &def, &locale_en_de()).unwrap();

        let cols = get_table_columns(&conn, "_global_settings").unwrap();
        assert!(cols.contains("site_name__en"));
        assert!(cols.contains("site_name__de"));
        assert!(!cols.contains("site_name"));
    }

    // ── global table alter with localized fields ────────────────────────

    #[test]
    fn global_table_alter_adds_localized_columns() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def1 = simple_global("settings", vec![text_field("name")]);
        sync_global_table(&conn, "settings", &def1, &locale_en_de()).unwrap();

        // Add a localized field to existing table
        let def2 = simple_global("settings", vec![
            text_field("name"),
            localized_field("description"),
        ]);
        sync_global_table(&conn, "settings", &def2, &locale_en_de()).unwrap();

        let cols = get_table_columns(&conn, "_global_settings").unwrap();
        assert!(cols.contains("description__en"));
        assert!(cols.contains("description__de"));
    }

    // ── global table with group fields ──────────────────────────────────

    #[test]
    fn global_table_group_fields_create() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_global("settings", vec![
            FieldDefinition {
                name: "seo".to_string(),
                field_type: FieldType::Group,
                fields: vec![text_field("title"), text_field("description")],
                ..Default::default()
            },
        ]);
        sync_global_table(&conn, "settings", &def, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "_global_settings").unwrap();
        assert!(cols.contains("seo__title"));
        assert!(cols.contains("seo__description"));
    }

    #[test]
    fn global_table_group_fields_alter() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def1 = simple_global("settings", vec![text_field("name")]);
        sync_global_table(&conn, "settings", &def1, &no_locale()).unwrap();

        // Add a group field to existing table
        let def2 = simple_global("settings", vec![
            text_field("name"),
            FieldDefinition {
                name: "seo".to_string(),
                field_type: FieldType::Group,
                fields: vec![text_field("title")],
                ..Default::default()
            },
        ]);
        sync_global_table(&conn, "settings", &def2, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "_global_settings").unwrap();
        assert!(cols.contains("seo__title"));
    }

    // ── global table with localized group fields ────────────────────────

    #[test]
    fn global_table_localized_group_create() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_global("settings", vec![
            FieldDefinition {
                name: "seo".to_string(),
                field_type: FieldType::Group,
                localized: true,
                fields: vec![text_field("title")],
                ..Default::default()
            },
        ]);
        sync_global_table(&conn, "settings", &def, &locale_en_de()).unwrap();

        let cols = get_table_columns(&conn, "_global_settings").unwrap();
        assert!(cols.contains("seo__title__en"));
        assert!(cols.contains("seo__title__de"));
    }

    #[test]
    fn global_table_localized_group_alter() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def1 = simple_global("settings", vec![text_field("name")]);
        sync_global_table(&conn, "settings", &def1, &locale_en_de()).unwrap();

        let def2 = simple_global("settings", vec![
            text_field("name"),
            FieldDefinition {
                name: "seo".to_string(),
                field_type: FieldType::Group,
                localized: true,
                fields: vec![text_field("title")],
                ..Default::default()
            },
        ]);
        sync_global_table(&conn, "settings", &def2, &locale_en_de()).unwrap();

        let cols = get_table_columns(&conn, "_global_settings").unwrap();
        assert!(cols.contains("seo__title__en"));
        assert!(cols.contains("seo__title__de"));
    }

    // ── versioned global table ──────────────────────────────────────────

    #[test]
    fn versioned_global_creates_versions_table() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let mut def = simple_global("settings", vec![text_field("name")]);
        def.versions = Some(VersionsConfig { drafts: true, max_versions: 5 });
        sync_global_table(&conn, "settings", &def, &no_locale()).unwrap();

        assert!(table_exists(&conn, "_versions__global_settings").unwrap());
        let cols = get_table_columns(&conn, "_global_settings").unwrap();
        assert!(cols.contains("_status"), "Drafts global should have _status column");
    }

    // ── global table alter adds _status for drafts ──────────────────────

    #[test]
    fn global_table_alter_adds_status_for_drafts() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def1 = simple_global("settings", vec![text_field("name")]);
        sync_global_table(&conn, "settings", &def1, &no_locale()).unwrap();

        let cols_before = get_table_columns(&conn, "_global_settings").unwrap();
        assert!(!cols_before.contains("_status"));

        // Now enable drafts
        let mut def2 = simple_global("settings", vec![text_field("name")]);
        def2.versions = Some(VersionsConfig { drafts: true, max_versions: 5 });
        sync_global_table(&conn, "settings", &def2, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "_global_settings").unwrap();
        assert!(cols.contains("_status"));
    }

    // ── global table with join tables ───────────────────────────────────

    #[test]
    fn global_table_creates_join_tables() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_global("settings", vec![
            FieldDefinition {
                name: "items".to_string(),
                field_type: FieldType::Array,
                fields: vec![text_field("label")],
                ..Default::default()
            },
        ]);
        sync_global_table(&conn, "settings", &def, &no_locale()).unwrap();

        assert!(table_exists(&conn, "_global_settings_items").unwrap());
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

    // ── localized has-many junction table ───────────────────────────────

    #[test]
    fn localized_has_many_creates_junction_with_locale() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![
            FieldDefinition {
                name: "tags".to_string(),
                field_type: FieldType::Relationship,
                localized: true,
                relationship: Some(RelationshipConfig {
                    collection: "tags".to_string(),
                    has_many: true,
                    max_depth: None,
                }),
                ..Default::default()
            },
        ]);
        create_collection_table(&conn, "posts", &def, &locale_en_de()).unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &locale_en_de()).unwrap();

        assert!(table_exists(&conn, "posts_tags").unwrap());
        let cols = get_table_columns(&conn, "posts_tags").unwrap();
        assert!(cols.contains("_locale"), "Localized junction table should have _locale column");
    }

    // ── localized array table ───────────────────────────────────────────

    #[test]
    fn localized_array_creates_table_with_locale() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![
            FieldDefinition {
                name: "items".to_string(),
                field_type: FieldType::Array,
                localized: true,
                fields: vec![text_field("label")],
                ..Default::default()
            },
        ]);
        create_collection_table(&conn, "posts", &def, &locale_en_de()).unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &locale_en_de()).unwrap();

        assert!(table_exists(&conn, "posts_items").unwrap());
        let cols = get_table_columns(&conn, "posts_items").unwrap();
        assert!(cols.contains("_locale"));
    }

    // ── localized blocks table ──────────────────────────────────────────

    #[test]
    fn localized_blocks_creates_table_with_locale() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![
            FieldDefinition {
                name: "content".to_string(),
                field_type: FieldType::Blocks,
                localized: true,
                ..Default::default()
            },
        ]);
        create_collection_table(&conn, "posts", &def, &locale_en_de()).unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &locale_en_de()).unwrap();

        assert!(table_exists(&conn, "posts_content").unwrap());
        let cols = get_table_columns(&conn, "posts_content").unwrap();
        assert!(cols.contains("_locale"));
    }

    // ── ensure_locale_column on existing table ──────────────────────────

    #[test]
    fn ensure_locale_column_adds_to_existing() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        conn.execute("CREATE TABLE test_join (parent_id TEXT, related_id TEXT)", []).unwrap();

        ensure_locale_column(&conn, "test_join", "en").unwrap();

        let cols = get_table_columns(&conn, "test_join").unwrap();
        assert!(cols.contains("_locale"));

        // Calling again should be a no-op (idempotent)
        ensure_locale_column(&conn, "test_join", "en").unwrap();
    }

    // ── existing localized join table adds _locale via alter ─────────────

    #[test]
    fn existing_has_many_adds_locale_column() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        // Create parent and junction table without _locale
        conn.execute("CREATE TABLE posts (id TEXT PRIMARY KEY)", []).unwrap();
        conn.execute("CREATE TABLE posts_tags (parent_id TEXT, related_id TEXT, _order INTEGER)", []).unwrap();

        let def = simple_collection("posts", vec![
            FieldDefinition {
                name: "tags".to_string(),
                field_type: FieldType::Relationship,
                localized: true,
                relationship: Some(RelationshipConfig {
                    collection: "tags".to_string(),
                    has_many: true,
                    max_depth: None,
                }),
                ..Default::default()
            },
        ]);
        sync_join_tables(&conn, "posts", &def.fields, &locale_en_de()).unwrap();

        let cols = get_table_columns(&conn, "posts_tags").unwrap();
        assert!(cols.contains("_locale"));
    }

    // ── existing array table adds sub-field columns and _locale ─────────

    #[test]
    fn existing_array_adds_new_subfield_columns() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        conn.execute("CREATE TABLE posts (id TEXT PRIMARY KEY)", []).unwrap();
        conn.execute("CREATE TABLE posts_items (id TEXT PRIMARY KEY, parent_id TEXT, _order INTEGER, label TEXT)", []).unwrap();

        let def = simple_collection("posts", vec![
            FieldDefinition {
                name: "items".to_string(),
                field_type: FieldType::Array,
                fields: vec![text_field("label"), text_field("value")],
                ..Default::default()
            },
        ]);
        sync_join_tables(&conn, "posts", &def.fields, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts_items").unwrap();
        assert!(cols.contains("value"), "New sub-field column should be added");
    }

    // ── existing blocks table adds _locale ──────────────────────────────

    #[test]
    fn existing_blocks_adds_locale_column() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        conn.execute("CREATE TABLE posts (id TEXT PRIMARY KEY)", []).unwrap();
        conn.execute(
            "CREATE TABLE posts_content (id TEXT PRIMARY KEY, parent_id TEXT, _order INTEGER, _block_type TEXT, data TEXT)",
            [],
        ).unwrap();

        let def = simple_collection("posts", vec![
            FieldDefinition {
                name: "content".to_string(),
                field_type: FieldType::Blocks,
                localized: true,
                ..Default::default()
            },
        ]);
        sync_join_tables(&conn, "posts", &def.fields, &locale_en_de()).unwrap();

        let cols = get_table_columns(&conn, "posts_content").unwrap();
        assert!(cols.contains("_locale"));
    }

    // ── existing localized array table adds _locale ─────────────────────

    #[test]
    fn existing_array_adds_locale_column() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        conn.execute("CREATE TABLE posts (id TEXT PRIMARY KEY)", []).unwrap();
        conn.execute("CREATE TABLE posts_items (id TEXT PRIMARY KEY, parent_id TEXT, _order INTEGER, label TEXT)", []).unwrap();

        let def = simple_collection("posts", vec![
            FieldDefinition {
                name: "items".to_string(),
                field_type: FieldType::Array,
                localized: true,
                fields: vec![text_field("label")],
                ..Default::default()
            },
        ]);
        sync_join_tables(&conn, "posts", &def.fields, &locale_en_de()).unwrap();

        let cols = get_table_columns(&conn, "posts_items").unwrap();
        assert!(cols.contains("_locale"));
    }

    // ── get_applied_migrations_desc with no table ──────────────────────

    #[test]
    fn get_applied_migrations_desc_no_table() {
        let pool = in_memory_pool();
        let result = get_applied_migrations_desc(&pool).unwrap();
        assert!(result.is_empty());
    }

    // ── get_applied_migrations_desc ordering ───────────────────────────

    #[test]
    fn get_applied_migrations_desc_ordering() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        conn.execute_batch(
            "CREATE TABLE _crap_migrations (filename TEXT PRIMARY KEY, applied_at TEXT DEFAULT (datetime('now')))"
        ).unwrap();
        record_migration(&conn, "001_a.lua").unwrap();
        record_migration(&conn, "002_b.lua").unwrap();
        record_migration(&conn, "003_c.lua").unwrap();

        let applied = get_applied_migrations_desc(&pool).unwrap();
        assert_eq!(applied, vec!["003_c.lua", "002_b.lua", "001_a.lua"]);
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
}
