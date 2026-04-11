//! Version restore operations for collections and globals.

use anyhow::{Context as _, Result, anyhow};
use serde_json::{Map, Value};
use std::collections::HashMap;

use crate::{
    config::LocaleConfig,
    core::{CollectionDefinition, Document, FieldDefinition, collection::GlobalDefinition},
    db::{
        DbConnection, DbValue,
        query::{
            LocaleContext, LocaleMode,
            global::update_global,
            helpers::{global_table, locale_column, prefixed_name, walk_leaf_fields},
            join::save_join_table_data,
            ref_count,
            write::update,
        },
    },
};

use super::{
    crud::{create_version, set_document_status},
    snapshot::{collect_join_data_from_snapshot, extract_snapshot_data},
};

/// Build a default-locale context when locales are enabled, or None otherwise.
fn default_locale_ctx(locale_config: &LocaleConfig) -> Option<LocaleContext> {
    locale_config.is_enabled().then(|| LocaleContext {
        mode: LocaleMode::Default,
        config: locale_config.clone(),
    })
}

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

    let locale_ctx = default_locale_ctx(locale_config);

    let old_refs =
        ref_count::snapshot_outgoing_refs(conn, slug, parent_id, &def.fields, locale_config)?;

    let doc = update(conn, slug, def, parent_id, &data, locale_ctx.as_ref())?;

    restore_locale_and_join_data(conn, slug, parent_id, &def.fields, obj, locale_config)?;

    // Adjust ref counts based on before/after diff
    ref_count::after_update(conn, slug, parent_id, &def.fields, locale_config, old_refs)?;

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

    let gtable = global_table(slug);
    let locales_enabled = locale_config.is_enabled();
    let data = extract_snapshot_data(obj, &def.fields, locales_enabled);

    let locale_ctx = default_locale_ctx(locale_config);

    let old_refs =
        ref_count::snapshot_outgoing_refs(conn, &gtable, "default", &def.fields, locale_config)?;

    let doc = update_global(conn, slug, def, &data, locale_ctx.as_ref())?;

    restore_locale_and_join_data(conn, &gtable, "default", &def.fields, obj, locale_config)?;

    // Adjust ref counts based on before/after diff
    ref_count::after_update(
        conn,
        &gtable,
        "default",
        &def.fields,
        locale_config,
        old_refs,
    )?;

    // Update status and create a new version for the restore
    set_document_status(conn, &gtable, "default", status)?;
    create_version(conn, &gtable, "default", status, snapshot)?;

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

    if locales_enabled {
        let mut set_clauses = Vec::new();
        let mut params: Vec<DbValue> = Vec::new();
        let mut idx = 1;

        collect_locale_restore_fields(
            conn,
            fields,
            obj,
            locale_config,
            &mut set_clauses,
            &mut params,
            &mut idx,
        )?;

        if !set_clauses.is_empty() {
            let sql = format!(
                "UPDATE \"{table}\" SET {} WHERE id = {}",
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
        save_join_table_data(conn, table, fields, parent_id, &join_data, None)?;
    }

    Ok(())
}

/// Resolve a snapshot value by trying the flat `"group__sub"` key first,
/// then navigating into the nested JSON object using the prefix segments.
fn resolve_snapshot_value<'a>(
    obj: &'a Map<String, Value>,
    base: &str,
    prefix: &str,
    field_name: &str,
) -> Option<&'a Value> {
    obj.get(base).or_else(|| {
        let parts: Vec<&str> = prefix.split("__").collect();
        let mut node: &Value = obj.get(parts[0])?;

        for part in &parts[1..] {
            node = node.as_object()?.get(*part)?;
        }

        node.as_object()?.get(field_name)
    })
}

/// Collect locale fields to restore using `walk_leaf_fields` to handle
/// Group/Row/Collapsible/Tabs recursion uniformly.
fn collect_locale_restore_fields(
    conn: &dyn DbConnection,
    fields: &[FieldDefinition],
    obj: &Map<String, Value>,
    locale_config: &LocaleConfig,
    set_clauses: &mut Vec<String>,
    params: &mut Vec<DbValue>,
    idx: &mut usize,
) -> Result<()> {
    walk_leaf_fields(
        fields,
        "",
        false,
        &mut |field, prefix, inherited_localized| {
            let is_localized = field.localized || inherited_localized;

            if !is_localized || !field.has_parent_column() {
                return Ok(());
            }

            let base = prefixed_name(prefix, &field.name);

            let val = if prefix.is_empty() {
                obj.get(&field.name)
            } else {
                resolve_snapshot_value(obj, &base, prefix, &field.name)
            };

            restore_locale_columns(conn, val, &base, locale_config, set_clauses, params, idx)
        },
    )
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
        let col = locale_column(field_name, locale)?;

        let db_val = if *locale == locale_config.default_locale {
            match snapshot_val {
                Some(Value::String(s)) => Some(DbValue::Text(s.clone())),
                Some(Value::Number(n)) => Some(DbValue::Text(n.to_string())),
                Some(Value::Bool(b)) => Some(DbValue::Integer(if *b { 1 } else { 0 })),
                _ => None,
            }
        } else {
            None
        };

        if let Some(val) = db_val {
            set_clauses.push(format!("{col} = {}", conn.placeholder(*idx)));
            params.push(val);
            *idx += 1;
        } else {
            set_clauses.push(format!("{col} = NULL"));
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
        FieldType,
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
