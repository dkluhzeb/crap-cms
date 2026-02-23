//! Dynamic schema migration: syncs SQLite tables to match Lua collection definitions.

use anyhow::{Context, Result};
use std::collections::HashSet;

use crate::core::SharedRegistry;
use crate::core::field::FieldType;
use super::DbPool;

/// Sync all collection tables with their Lua definitions.
pub fn sync_all(pool: &DbPool, registry: &SharedRegistry) -> Result<()> {
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

    let reg = registry.read()
        .map_err(|e| anyhow::anyhow!("Registry lock poisoned: {}", e))?;

    for (slug, def) in &reg.collections {
        sync_collection_table(&tx, slug, def)?;
    }

    for (slug, def) in &reg.globals {
        sync_global_table(&tx, slug, def)?;
    }

    drop(reg);
    tx.commit().context("Failed to commit migration transaction")?;

    Ok(())
}

fn sync_collection_table(
    conn: &rusqlite::Connection,
    slug: &str,
    def: &crate::core::CollectionDefinition,
) -> Result<()> {
    let table_exists = table_exists(conn, slug)?;

    if !table_exists {
        create_collection_table(conn, slug, def)?;
    } else {
        alter_collection_table(conn, slug, def)?;
    }

    // Sync join tables for has-many relationships and array fields
    sync_join_tables(conn, slug, &def.fields)?;

    Ok(())
}

fn sync_global_table(
    conn: &rusqlite::Connection,
    slug: &str,
    def: &crate::core::collection::GlobalDefinition,
) -> Result<()> {
    let table_name = format!("_global_{}", slug);
    let table_exists = table_exists(conn, &table_name)?;

    if !table_exists {
        let mut columns = vec!["id TEXT PRIMARY KEY".to_string()];

        for field in &def.fields {
            let col = format!("{} {}", field.name, field.field_type.sqlite_type());
            columns.push(col);
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
) -> Result<()> {
    let mut columns = vec!["id TEXT PRIMARY KEY".to_string()];

    for field in &def.fields {
        // Skip fields that use join tables (array, has-many relationship)
        if !field.has_parent_column() {
            continue;
        }
        let mut col = format!("{} {}", field.name, field.field_type.sqlite_type());
        if field.required {
            col.push_str(" NOT NULL");
        }
        if field.unique {
            col.push_str(" UNIQUE");
        }
        if let Some(ref default) = field.default_value {
            match default {
                serde_json::Value::String(s) => col.push_str(&format!(" DEFAULT '{}'", s)),
                serde_json::Value::Number(n) => col.push_str(&format!(" DEFAULT {}", n)),
                serde_json::Value::Bool(b) => col.push_str(&format!(" DEFAULT {}", if *b { 1 } else { 0 })),
                _ => {}
            }
        } else if field.field_type == FieldType::Checkbox {
            col.push_str(" DEFAULT 0");
        }
        columns.push(col);
    }

    // Auth collections get hidden system columns (not regular fields)
    if def.is_auth_collection() {
        columns.push("_password_hash TEXT".to_string());
        columns.push("_reset_token TEXT".to_string());
        columns.push("_reset_token_exp INTEGER".to_string());
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
) -> Result<()> {
    // Get existing columns
    let existing_columns = get_table_columns(conn, slug)?;

    for field in &def.fields {
        // Skip fields that use join tables (array, has-many relationship)
        if !field.has_parent_column() {
            continue;
        }
        if !existing_columns.contains(&field.name) {
            let mut col_def = field.field_type.sqlite_type().to_string();
            if let Some(ref default) = field.default_value {
                match default {
                    serde_json::Value::String(s) => col_def.push_str(&format!(" DEFAULT '{}'", s)),
                    serde_json::Value::Number(n) => col_def.push_str(&format!(" DEFAULT {}", n)),
                    serde_json::Value::Bool(b) => col_def.push_str(&format!(" DEFAULT {}", if *b { 1 } else { 0 })),
                    _ => {}
                }
            } else if field.field_type == FieldType::Checkbox {
                col_def.push_str(" DEFAULT 0");
            }

            let sql = format!("ALTER TABLE {} ADD COLUMN {} {}", slug, field.name, col_def);
            tracing::info!("Adding column to {}: {}", slug, field.name);
            conn.execute(&sql, [])
                .with_context(|| format!("Failed to add column {} to {}", field.name, slug))?;
        }
    }

    // Auth collections: ensure system columns exist
    if def.is_auth_collection() {
        for col in ["_password_hash TEXT", "_reset_token TEXT", "_reset_token_exp INTEGER"] {
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

    // Warn about removed columns (SQLite can't DROP COLUMN easily)
    let field_names: HashSet<String> = def.fields.iter()
        .filter(|f| f.has_parent_column())
        .map(|f| f.name.clone())
        .collect();
    let system_columns: HashSet<&str> = [
        "id", "created_at", "updated_at", "_password_hash",
        "_reset_token", "_reset_token_exp", "_verified", "_verification_token",
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
) -> Result<()> {
    use crate::core::field::FieldType;

    for field in fields {
        match field.field_type {
            FieldType::Relationship => {
                if let Some(ref rc) = field.relationship {
                    if rc.has_many {
                        let table_name = format!("{}_{}", collection_slug, field.name);
                        if !table_exists(conn, &table_name)? {
                            let sql = format!(
                                "CREATE TABLE {} (\
                                    parent_id TEXT NOT NULL REFERENCES {}(id) ON DELETE CASCADE, \
                                    related_id TEXT NOT NULL, \
                                    _order INTEGER NOT NULL DEFAULT 0, \
                                    PRIMARY KEY (parent_id, related_id)\
                                )",
                                table_name, collection_slug
                            );
                            tracing::info!("Creating junction table: {}", table_name);
                            conn.execute(&sql, [])
                                .with_context(|| format!("Failed to create junction table {}", table_name))?;
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
            _ => {}
        }
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
