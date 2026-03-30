//! Version restore operations for collections and globals.

use anyhow::{Context as _, Result, anyhow};
use serde_json::{Map, Value};
use std::collections::HashMap;

use crate::{
    config::LocaleConfig,
    core::{
        CollectionDefinition, Document, FieldDefinition, FieldType, collection::GlobalDefinition,
    },
    db::{DbConnection, DbValue, query::sanitize_locale},
};

use super::{
    crud::{create_version, set_document_status},
    snapshot::{collect_join_data_from_snapshot, extract_snapshot_data},
};

/// Restore a version snapshot back to the main table. Updates all regular columns
/// and join tables from the snapshot data. Creates a new version recording the restore.
///
/// When `locale_config` indicates locales are enabled, localized fields are handled
/// specially: ALL locale columns are cleared, then the snapshot value is written to
/// the default locale column. This ensures stale translations from later edits don't
/// persist after restoring an older version.
pub fn restore_version(
    conn: &dyn DbConnection,
    slug: &str,
    def: &CollectionDefinition,
    parent_id: &str,
    snapshot: &Value,
    status: &str,
    locale_config: &LocaleConfig,
) -> Result<Document> {
    let obj = snapshot
        .as_object()
        .ok_or_else(|| anyhow!("Snapshot is not a JSON object"))?;

    let locales_enabled = locale_config.is_enabled();
    let data = extract_snapshot_data(obj, &def.fields, locales_enabled);

    // When locales are enabled, use a default locale context so that update()'s
    // internal find_by_id can read back columns with locale suffixes.
    let locale_ctx = if locales_enabled {
        Some(super::super::LocaleContext {
            mode: super::super::LocaleMode::Default,
            config: locale_config.clone(),
        })
    } else {
        None
    };

    // Snapshot outgoing refs before restore for ref count adjustment
    let old_refs = super::super::ref_count::snapshot_outgoing_refs(
        conn,
        slug,
        parent_id,
        &def.fields,
        locale_config,
    )?;

    let doc = super::super::update(conn, slug, def, parent_id, &data, locale_ctx.as_ref())?;

    restore_locale_and_join_data(conn, slug, parent_id, &def.fields, obj, locale_config)?;

    // Adjust ref counts based on before/after diff
    super::super::ref_count::after_update(
        conn,
        slug,
        parent_id,
        &def.fields,
        locale_config,
        old_refs,
    )?;

    // Update status and create a new version for the restore
    set_document_status(conn, slug, parent_id, status)?;
    create_version(conn, slug, parent_id, status, snapshot)?;

    Ok(doc)
}

/// Restore a version snapshot back to a global's main table.
/// Group fields use expanded `field__subfield` sub-columns (same as collections).
pub fn restore_global_version(
    conn: &dyn DbConnection,
    slug: &str,
    def: &GlobalDefinition,
    snapshot: &Value,
    status: &str,
    locale_config: &LocaleConfig,
) -> Result<Document> {
    let obj = snapshot
        .as_object()
        .ok_or_else(|| anyhow!("Snapshot is not a JSON object"))?;

    let global_table = format!("_global_{}", slug);
    let locales_enabled = locale_config.is_enabled();
    let data = extract_snapshot_data(obj, &def.fields, locales_enabled);

    let locale_ctx = if locales_enabled {
        Some(super::super::LocaleContext {
            mode: super::super::LocaleMode::Default,
            config: locale_config.clone(),
        })
    } else {
        None
    };

    // Snapshot outgoing refs before restore for ref count adjustment
    let old_refs = super::super::ref_count::snapshot_outgoing_refs(
        conn,
        &global_table,
        "default",
        &def.fields,
        locale_config,
    )?;

    let doc = super::super::update_global(conn, slug, def, &data, locale_ctx.as_ref())?;

    restore_locale_and_join_data(
        conn,
        &global_table,
        "default",
        &def.fields,
        obj,
        locale_config,
    )?;

    // Adjust ref counts based on before/after diff
    super::super::ref_count::after_update(
        conn,
        &global_table,
        "default",
        &def.fields,
        locale_config,
        old_refs,
    )?;

    // Update status and create a new version for the restore
    set_document_status(conn, &global_table, "default", status)?;
    create_version(conn, &global_table, "default", status, snapshot)?;

    Ok(doc)
}

/// Restore locale columns and join table data from a snapshot.
/// Group fields are always expanded to `field__subfield` sub-columns.
fn restore_locale_and_join_data(
    conn: &dyn DbConnection,
    table: &str,
    parent_id: &str,
    fields: &[FieldDefinition],
    obj: &Map<String, Value>,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let locales_enabled = locale_config.is_enabled();

    // Restore localized main-table columns: clear ALL locale columns, set default from snapshot.
    if locales_enabled {
        let mut set_clauses = Vec::new();
        let mut params: Vec<DbValue> = Vec::new();
        let mut idx = 1;

        for field in fields {
            if field.field_type == FieldType::Group {
                let nested_obj = obj.get(&field.name).and_then(|v| v.as_object());
                for sub in &field.fields {
                    let is_localized = field.localized || sub.localized;

                    if !is_localized {
                        continue;
                    }
                    let base = format!("{}__{}", field.name, sub.name);
                    // Resolve value from flat key or nested path
                    let val = obj
                        .get(&base)
                        .or_else(|| nested_obj.and_then(|n| n.get(&sub.name)));
                    restore_locale_columns(
                        conn,
                        val,
                        &base,
                        locale_config,
                        &mut set_clauses,
                        &mut params,
                        &mut idx,
                    )?;
                }
                continue;
            }
            // Row/Collapsible fields promote sub-fields as top-level columns (no prefix).
            // Recurse to handle nested layout wrappers.
            if field.field_type == FieldType::Row || field.field_type == FieldType::Collapsible {
                collect_locale_restore_fields(
                    conn,
                    &field.fields,
                    obj,
                    locale_config,
                    &mut set_clauses,
                    &mut params,
                    &mut idx,
                )?;
                continue;
            }
            // Tabs fields promote sub-fields from all tabs as top-level columns (no prefix).
            // Recurse to handle nested layout wrappers.
            if field.field_type == FieldType::Tabs {
                for tab in &field.tabs {
                    collect_locale_restore_fields(
                        conn,
                        &tab.fields,
                        obj,
                        locale_config,
                        &mut set_clauses,
                        &mut params,
                        &mut idx,
                    )?;
                }
                continue;
            }
            if !field.localized || !field.has_parent_column() {
                continue;
            }
            restore_locale_columns(
                conn,
                obj.get(&field.name),
                &field.name,
                locale_config,
                &mut set_clauses,
                &mut params,
                &mut idx,
            )?;
        }

        if !set_clauses.is_empty() {
            let sql = format!(
                "UPDATE \"{}\" SET {} WHERE id = {}",
                table,
                set_clauses.join(", "),
                conn.placeholder(idx)
            );
            params.push(DbValue::Text(parent_id.to_string()));
            conn.execute(&sql, &params)
                .context("Failed to restore locale columns")?;
        }
    }

    // Restore join table data from snapshot
    let mut join_data: HashMap<String, Value> = HashMap::new();
    collect_join_data_from_snapshot(fields, obj, &mut join_data);

    if !join_data.is_empty() {
        super::super::save_join_table_data(conn, table, fields, parent_id, &join_data, None)?;
    }

    Ok(())
}

/// Recursively collect locale fields to restore from layout wrappers (Row/Collapsible/Tabs).
fn collect_locale_restore_fields(
    conn: &dyn DbConnection,
    fields: &[FieldDefinition],
    obj: &Map<String, Value>,
    locale_config: &LocaleConfig,
    set_clauses: &mut Vec<String>,
    params: &mut Vec<DbValue>,
    idx: &mut usize,
) -> Result<()> {
    for field in fields {
        if field.field_type == FieldType::Group {
            let nested_obj = obj.get(&field.name).and_then(|v| v.as_object());
            for sub in &field.fields {
                let is_localized = field.localized || sub.localized;

                if !is_localized {
                    continue;
                }
                let base = format!("{}__{}", field.name, sub.name);
                let val = obj
                    .get(&base)
                    .or_else(|| nested_obj.and_then(|n| n.get(&sub.name)));
                restore_locale_columns(conn, val, &base, locale_config, set_clauses, params, idx)?;
            }
        } else if field.field_type == FieldType::Row || field.field_type == FieldType::Collapsible {
            collect_locale_restore_fields(
                conn,
                &field.fields,
                obj,
                locale_config,
                set_clauses,
                params,
                idx,
            )?;
        } else if field.field_type == FieldType::Tabs {
            for tab in &field.tabs {
                collect_locale_restore_fields(
                    conn,
                    &tab.fields,
                    obj,
                    locale_config,
                    set_clauses,
                    params,
                    idx,
                )?;
            }
        } else if field.localized && field.has_parent_column() {
            restore_locale_columns(
                conn,
                obj.get(&field.name),
                &field.name,
                locale_config,
                set_clauses,
                params,
                idx,
            )?;
        }
    }

    Ok(())
}

/// Emit SET clauses that NULL all locale columns for a field, then set the
/// default locale column to the snapshot value.
fn restore_locale_columns(
    conn: &dyn DbConnection,
    snapshot_val: Option<&Value>,
    field_name: &str,
    locale_config: &LocaleConfig,
    set_clauses: &mut Vec<String>,
    params: &mut Vec<DbValue>,
    idx: &mut usize,
) -> Result<()> {
    for locale in &locale_config.locales {
        let col = format!("{}__{}", field_name, sanitize_locale(locale)?);

        if *locale == locale_config.default_locale {
            // Set default locale from snapshot
            match snapshot_val {
                Some(Value::String(s)) => {
                    set_clauses.push(format!("{} = {}", col, conn.placeholder(*idx)));
                    params.push(DbValue::Text(s.clone()));
                    *idx += 1;
                }
                Some(Value::Number(n)) => {
                    set_clauses.push(format!("{} = {}", col, conn.placeholder(*idx)));
                    params.push(DbValue::Text(n.to_string()));
                    *idx += 1;
                }
                Some(Value::Bool(b)) => {
                    set_clauses.push(format!("{} = {}", col, conn.placeholder(*idx)));
                    params.push(DbValue::Integer(if *b { 1 } else { 0 }));
                    *idx += 1;
                }
                _ => {
                    set_clauses.push(format!("{} = NULL", col));
                }
            }
        } else {
            // Clear non-default locale columns
            set_clauses.push(format!("{} = NULL", col));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::config::{CrapConfig, LocaleConfig};
    use crate::core::{
        collection::{CollectionDefinition, VersionsConfig},
        field::{FieldDefinition, FieldTab},
    };
    use crate::db::{BoxedConnection, pool, query::versions::crud::count_versions};
    use tempfile::TempDir;

    fn setup_conn() -> (TempDir, BoxedConnection) {
        let dir = TempDir::new().unwrap();
        let config = CrapConfig::default();
        let db_pool = pool::create_pool(dir.path(), &config).unwrap();
        let conn = db_pool.get().unwrap();
        (dir, conn)
    }

    #[test]
    fn restore_version_localized_blocks_inside_tabs() {
        // Regression: restore_locale_and_join_data tried to SET locale columns for
        // blocks fields inside Tabs (which don't have parent columns), causing SQL error.
        let (_dir, conn) = setup_conn();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                title__en TEXT,
                title__de TEXT,
                _status TEXT DEFAULT 'published',
                created_at TEXT DEFAULT (datetime('now')),
                updated_at TEXT DEFAULT (datetime('now'))
            );
            CREATE TABLE posts_content (
                id TEXT PRIMARY KEY,
                parent_id TEXT,
                _order INTEGER,
                _block_type TEXT,
                data TEXT
            );
            CREATE TABLE _versions_posts (
                id TEXT PRIMARY KEY,
                _parent TEXT NOT NULL,
                _version INTEGER NOT NULL,
                _status TEXT NOT NULL,
                _latest INTEGER NOT NULL DEFAULT 0,
                snapshot TEXT NOT NULL,
                created_at TEXT DEFAULT (datetime('now')),
                updated_at TEXT DEFAULT (datetime('now'))
            );
            INSERT INTO posts (id, title__en, title__de, _status) VALUES ('p1', 'Hello', 'Hallo', 'published');"
        ).unwrap();

        let locale_config = LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: true,
        };

        let blocks_field = FieldDefinition::builder("content", FieldType::Blocks)
            .localized(true)
            .build();
        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text)
                .localized(true)
                .build(),
            FieldDefinition::builder("page_settings", FieldType::Tabs)
                .tabs(vec![FieldTab::new("Content", vec![blocks_field])])
                .build(),
        ];
        def.versions = Some(VersionsConfig::new(true, 10));
        let def = def;

        let snapshot = json!({
            "title": "Restored Title",
            "content": [
                {"_block_type": "hero", "heading": "Welcome back"}
            ]
        });

        // This should NOT fail with "Failed to restore locale columns"
        let doc = restore_version(
            &conn,
            "posts",
            &def,
            "p1",
            &snapshot,
            "published",
            &locale_config,
        )
        .unwrap();
        assert_eq!(doc.id, "p1");

        // Verify title was restored to default locale
        let row = conn
            .query_one("SELECT title__en FROM posts WHERE id = 'p1'", &[])
            .unwrap()
            .unwrap();
        let title = row.get_string("title__en").unwrap();
        assert_eq!(title, "Restored Title");

        // Verify blocks were restored to join table
        let row = conn
            .query_one(
                "SELECT COUNT(*) AS cnt FROM posts_content WHERE parent_id = 'p1'",
                &[],
            )
            .unwrap()
            .unwrap();
        let block_count = row.get_i64("cnt").unwrap();
        assert_eq!(block_count, 1, "blocks from snapshot should be restored");

        // Verify a version was created for the restore
        let version_count = count_versions(&conn, "posts", "p1").unwrap();
        assert_eq!(version_count, 1);
    }

    #[test]
    fn restore_version_preserves_timezone_data() {
        // Regression: version snapshots must include _tz companion columns
        // and restoring a version must write them back.
        let (_dir, conn) = setup_conn();
        conn.execute_batch(
            "CREATE TABLE events (
                id TEXT PRIMARY KEY,
                start_date TEXT,
                start_date_tz TEXT,
                _status TEXT DEFAULT 'published',
                created_at TEXT DEFAULT (datetime('now')),
                updated_at TEXT DEFAULT (datetime('now'))
            );
            CREATE TABLE _versions_events (
                id TEXT PRIMARY KEY,
                _parent TEXT NOT NULL,
                _version INTEGER NOT NULL,
                _status TEXT NOT NULL,
                _latest INTEGER NOT NULL DEFAULT 0,
                snapshot TEXT NOT NULL,
                created_at TEXT DEFAULT (datetime('now')),
                updated_at TEXT DEFAULT (datetime('now'))
            );
            INSERT INTO events (id, start_date, start_date_tz, _status)
                VALUES ('e1', '2024-06-15T14:00:00.000Z', 'America/New_York', 'published');",
        )
        .unwrap();

        let no_locale = LocaleConfig::default();
        let mut def = CollectionDefinition::new("events");
        def.fields = vec![
            FieldDefinition::builder("start_date", FieldType::Date)
                .timezone(true)
                .build(),
        ];
        def.versions = Some(VersionsConfig::new(true, 10));

        // Create a snapshot that includes both the date and timezone
        let snapshot_v1 = json!({
            "start_date": "2024-06-15T14:00:00.000Z",
            "start_date_tz": "America/New_York"
        });
        create_version(&conn, "events", "e1", "published", &snapshot_v1).unwrap();

        // Simulate updating the document with a different timezone
        conn.execute_batch(
            "UPDATE events SET start_date = '2024-06-15T18:00:00.000Z', \
             start_date_tz = 'Europe/London' WHERE id = 'e1'",
        )
        .unwrap();

        // Verify the update took effect
        let row = conn
            .query_one("SELECT start_date_tz FROM events WHERE id = 'e1'", &[])
            .unwrap()
            .unwrap();
        assert_eq!(row.get_string("start_date_tz").unwrap(), "Europe/London");

        // Restore the original version
        let doc = restore_version(
            &conn,
            "events",
            &def,
            "e1",
            &snapshot_v1,
            "published",
            &no_locale,
        )
        .unwrap();

        // Verify the restored document has the original date
        assert_eq!(
            doc.get_str("start_date"),
            Some("2024-06-15T14:00:00.000Z"),
            "Restored date should match the snapshot"
        );

        // Verify the _tz column was also restored by reading directly from the DB
        let row = conn
            .query_one("SELECT start_date_tz FROM events WHERE id = 'e1'", &[])
            .unwrap()
            .unwrap();
        assert_eq!(
            row.get_string("start_date_tz").unwrap(),
            "America/New_York",
            "Restored timezone should match the snapshot"
        );
    }
}
