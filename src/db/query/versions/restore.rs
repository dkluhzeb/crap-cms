//! Version restore operations for collections and globals.

use anyhow::{Context as _, Result, anyhow};
use serde_json::{Map, Value};

use crate::{
    config::LocaleConfig,
    core::{
        CollectionDefinition, Document, DocumentFields, FieldDefinition, FieldType,
        collection::GlobalDefinition, flatten_group_fields,
    },
    db::{
        DbConnection, DbValue,
        query::{
            LocaleContext, LocaleMode,
            fts::fts_upsert,
            global::update_global,
            helpers::{
                coerce_has_many_scalar, coerce_json_value, global_table, locale_column,
                prefixed_name, quote_ident, tz_column, walk_leaf_fields,
            },
            join::save_join_table_data,
            ref_count,
            write::update,
        },
    },
};

use super::{
    crud::{create_version, set_document_status},
    snapshot::{collect_join_data_from_snapshot, extract_snapshot_data, localized_join_keys},
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
/// When `locale_config` indicates locales are enabled, every locale column of a
/// localized field takes the snapshot's value for that locale, and a locale the
/// snapshot has no value for is cleared — so translations from later edits don't
/// survive restoring an older version. A field the snapshot carries no value for
/// at all keeps its stored columns.
///
/// # Errors
///
/// Returns an error if the snapshot is not a JSON object, or a backend error
/// if the UPDATE / join-table sync / version creation fails.
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

    // Row lock before the unlocked ref snapshot (see `persist_update`).
    conn.lock_row(slug, parent_id)?;
    let old_refs =
        ref_count::snapshot_outgoing_refs(conn, slug, parent_id, &def.fields, locale_config)?;

    let doc = update(conn, slug, def, parent_id, &data, locale_ctx.as_ref())?;

    restore_locale_and_join_data(conn, slug, parent_id, &def.fields, obj, locale_config)?;

    // Adjust ref counts based on before/after diff
    ref_count::after_update(conn, slug, parent_id, &def.fields, locale_config, &old_refs)?;

    // Re-sync the FTS index to the restored content.
    fts_upsert(conn, slug, parent_id, def, locale_config)?;

    // `_status` only exists when the collection has drafts — an audit-trail
    // collection (`versions = { drafts = false }`) has no such column, and
    // writing it would fail the restore with a raw backend error.
    if def.has_drafts() {
        set_document_status(conn, slug, parent_id, status)?;
    }
    create_version(conn, slug, parent_id, status, snapshot)?;

    Ok(doc)
}

/// Restore a version snapshot back to a global's main table.
/// Group fields use expanded `field__subfield` sub-columns (same as collections).
///
/// # Errors
///
/// Returns an error if the snapshot is not a JSON object, or a backend error
/// if the UPDATE / join-table sync / version creation fails.
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

    conn.lock_row(&gtable, "default")?;
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
        &old_refs,
    )?;

    if def.has_drafts() {
        set_document_status(conn, &gtable, "default", status)?;
    }
    create_version(conn, &gtable, "default", status, snapshot)?;

    Ok(doc)
}

/// The row a restore writes back to.
struct RestoreRow<'a> {
    conn: &'a dyn DbConnection,
    table: &'a str,
    parent_id: &'a str,
    fields: &'a [FieldDefinition],
}

/// The SET clauses of one UPDATE and the values they bind.
struct SetClauses<'a> {
    conn: &'a dyn DbConnection,
    clauses: Vec<String>,
    params: Vec<DbValue>,
}

impl SetClauses<'_> {
    /// Set `column` to `value`, or to NULL without one.
    fn push(&mut self, column: &str, value: Option<DbValue>) {
        let quoted = quote_ident(column);

        let Some(value) = value else {
            self.clauses.push(format!("{quoted} = NULL"));
            return;
        };

        let placeholder = self.conn.placeholder(self.params.len() + 1);
        self.clauses.push(format!("{quoted} = {placeholder}"));
        self.params.push(value);
    }
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
    let row = RestoreRow {
        conn,
        table,
        parent_id,
        fields,
    };

    if locale_config.is_enabled() {
        restore_locale_values(&row, obj, locale_config)?;
    }

    restore_join_rows(&row, obj, locale_config)
}

/// Write every localized column back from the snapshot in one UPDATE.
fn restore_locale_values(
    row: &RestoreRow<'_>,
    obj: &Map<String, Value>,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let mut set = SetClauses {
        conn: row.conn,
        clauses: Vec::new(),
        params: Vec::new(),
    };

    collect_locale_restore_fields(&mut set, row.fields, obj, locale_config)?;

    if set.clauses.is_empty() {
        return Ok(());
    }

    let sql = format!(
        "UPDATE \"{}\" SET {} WHERE id = {}",
        row.table,
        set.clauses.join(", "),
        row.conn.placeholder(set.params.len() + 1)
    );
    set.params.push(DbValue::Text(row.parent_id.to_string()));

    row.conn
        .execute(&sql, &set.params)
        .context("Failed to restore locale columns")?;

    Ok(())
}

/// Restore join table data from the snapshot. Localized join fields are left
/// to the per-locale pass: written here without a locale, every locale's rows
/// would land in the default locale.
fn restore_join_rows(
    row: &RestoreRow<'_>,
    obj: &Map<String, Value>,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let mut collected = DocumentFields::new();
    collect_join_data_from_snapshot(row.fields, obj, &mut collected);
    let mut join_data = flatten_group_fields(&collected, row.fields);

    let localized_keys = if locale_config.is_enabled() {
        localized_join_keys(row.fields)
    } else {
        Vec::new()
    };
    for key in &localized_keys {
        join_data.remove(key);
    }

    if !join_data.is_empty() {
        save_join_table_data(
            row.conn,
            row.table,
            row.fields,
            row.parent_id,
            &join_data,
            None,
        )?;
    }

    restore_localized_join_rows(row, obj, locale_config, &localized_keys)
}

/// Write each locale's rows of every localized join field back from its
/// `{key}__{locale}` snapshot entry (the locale code in column form), scoped to
/// that locale. A snapshot without the entry for a field — taken before
/// snapshots recorded localized rows per locale — leaves that field's live rows
/// untouched rather than guessing.
fn restore_localized_join_rows(
    row: &RestoreRow<'_>,
    obj: &Map<String, Value>,
    locale_config: &LocaleConfig,
    keys: &[String],
) -> Result<()> {
    for locale in &locale_config.locales {
        let mut data = DocumentFields::new();

        for key in keys {
            if let Some(rows) = obj.get(&locale_column(key, locale)?) {
                data.insert(key.clone(), rows.clone());
            }
        }

        if data.is_empty() {
            continue;
        }

        let ctx = LocaleContext {
            mode: LocaleMode::Single(locale.clone()),
            config: locale_config.clone(),
        };
        save_join_table_data(
            row.conn,
            row.table,
            row.fields,
            row.parent_id,
            &data,
            Some(&ctx),
        )?;
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

/// Where a localized value sits in a snapshot: its flat column name
/// (`group__field`), the group prefix (`group`, empty at the top level) and the
/// field's own name.
type SnapshotKey<'k> = (&'k str, &'k str, &'k str);

/// A snapshot's per-locale values.
struct LocaleSnapshot<'a> {
    obj: &'a Map<String, Value>,
    config: &'a LocaleConfig,
}

impl<'a> LocaleSnapshot<'a> {
    fn new(obj: &'a Map<String, Value>, config: &'a LocaleConfig) -> Self {
        Self { obj, config }
    }

    /// The value of `key` for `locale`. EVERY locale prefers the decorated key
    /// the snapshot carries — `{key}__{locale}`, the locale code in column
    /// form. The bare key is only the default locale's fallback, for snapshots
    /// written before snapshots recorded every locale: it holds whichever
    /// locale the write that produced it was made under, so preferring it would
    /// copy (say) a German edit into the English column on restore.
    fn value(
        &self,
        (base, prefix, field): SnapshotKey<'_>,
        locale: &str,
    ) -> Result<Option<&'a Value>> {
        let decorated_base = locale_column(base, locale)?;
        let decorated_field = locale_column(field, locale)?;

        if let Some(value) =
            resolve_snapshot_value(self.obj, &decorated_base, prefix, &decorated_field)
        {
            return Ok(Some(value));
        }

        if locale != self.config.default_locale {
            return Ok(None);
        }

        Ok(resolve_snapshot_value(self.obj, base, prefix, field))
    }

    /// Whether the snapshot carries a value of `key` for any locale.
    fn carries(&self, key: SnapshotKey<'_>) -> Result<bool> {
        for locale in &self.config.locales {
            if self.value(key, locale)?.is_some() {
                return Ok(true);
            }
        }

        Ok(false)
    }
}

/// Collect locale fields to restore using `walk_leaf_fields` to handle
/// Group/Row/Collapsible/Tabs recursion uniformly.
fn collect_locale_restore_fields(
    set: &mut SetClauses<'_>,
    fields: &[FieldDefinition],
    obj: &Map<String, Value>,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let snapshot = LocaleSnapshot::new(obj, locale_config);

    walk_leaf_fields(
        fields,
        "",
        false,
        &mut |field, prefix, inherited_localized| {
            if !(field.localized || inherited_localized) || !field.has_parent_column() {
                return Ok(());
            }

            let base = prefixed_name(prefix, &field.name);
            let key = (base.as_str(), prefix, field.name.as_str());

            // A field the snapshot carries no value for at all — no bare key and
            // no locale column — was either removed from the restore (the
            // caller is write-denied on it) or did not exist when the snapshot
            // was taken. Leave its stored columns alone, exactly as a missing
            // non-localized field is left alone, instead of NULLing every
            // translation.
            if !snapshot.carries(key)? {
                return Ok(());
            }

            restore_locale_columns(set, &snapshot, key, |v| locale_column_value(field, v))?;

            // A timezone-enabled Date carries a `{base}_tz` companion, localized
            // the same way and present in the snapshot (the locale SELECT emits
            // it). Restore its per-locale values too, with the same lookup
            // order — otherwise restoring an old version leaves the timezone(s)
            // at the current post-edit value.
            if field.has_tz_companion() {
                let (tz_base, tz_field) = (tz_column(&base), tz_column(&field.name));
                let tz_key = (tz_base.as_str(), prefix, tz_field.as_str());

                restore_locale_columns(set, &snapshot, tz_key, |v| {
                    coerce_json_value(&FieldType::Text, v)
                })?;
            }

            Ok(())
        },
    )
}

/// Emit SET clauses restoring every locale column of a field from the
/// snapshot: each locale takes the snapshot's value for that locale, and a
/// locale the snapshot has no value for is set to NULL. Restoring used to NULL
/// every non-default locale even though snapshots carry the decorated `__xx`
/// values — wiping translations.
fn restore_locale_columns(
    set: &mut SetClauses<'_>,
    snapshot: &LocaleSnapshot<'_>,
    key: SnapshotKey<'_>,
    column_value: impl Fn(&Value) -> DbValue,
) -> Result<()> {
    for locale in &snapshot.config.locales {
        let col = locale_column(key.0, locale)?;

        let db_val = snapshot
            .value(key, locale)?
            .map(&column_value)
            .filter(|v| !v.is_null());

        set.push(&col, db_val);
    }

    Ok(())
}

/// A localized column's value from its snapshot value, coerced as a write
/// coerces it — a scalar has-many list to its JSON text, anything else by type.
/// A snapshot taken before email and text were stored canonically holds the
/// value as typed; the restore writes the stored form.
fn locale_column_value(field: &FieldDefinition, value: &Value) -> DbValue {
    if field.is_has_many_scalar() {
        return coerce_has_many_scalar(&field.field_type, value);
    }

    coerce_json_value(&field.field_type, value)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;

    use super::*;
    use crate::config::{CrapConfig, LocaleConfig};
    use crate::core::{CollectionDefinition, FieldDefinition, FieldTab, FieldType, VersionsConfig};
    use crate::db::query::join::{find_array_rows, set_array_rows};
    use crate::db::{
        BoxedConnection, pool,
        query::versions::{build_snapshot, crud::count_versions},
    };
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
                data TEXT,
                _locale TEXT
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
            ],
            "content__en": [
                {"_block_type": "hero", "heading": "Welcome back"}
            ],
            "content__de": [
                {"_block_type": "hero", "heading": "Willkommen zurück"},
                {"_block_type": "hero", "heading": "Nochmals"}
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

        // Verify each locale's blocks were restored to the join table
        let count_blocks = |locale: &str| {
            conn.query_one(
                "SELECT COUNT(*) AS cnt FROM posts_content WHERE parent_id = 'p1' AND _locale = ?1",
                &[DbValue::Text(locale.to_string())],
            )
            .unwrap()
            .unwrap()
            .get_i64("cnt")
            .unwrap()
        };
        assert_eq!(count_blocks("en"), 1, "english blocks should be restored");
        assert_eq!(count_blocks("de"), 2, "german blocks should be restored");

        // Verify a version was created for the restore
        let version_count = count_versions(&conn, "posts", "p1", false).unwrap();
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

    /// Restoring a version is a column-preserving write for array rows too: a
    /// field the restoring user cannot write is dropped from the snapshot by the
    /// service-layer strip, and — because the snapshot carries each array row's
    /// `id` (hydration includes it) — the diff-based join writer matches the
    /// live row by that id and leaves the stripped field at its live value,
    /// rather than resurrecting the snapshot value or clearing it. This pins the
    /// restore end of the row-identity contract.
    #[test]
    fn restore_version_preserves_array_subfield_omitted_by_strip() {
        let (_dir, conn) = setup_conn();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                _status TEXT DEFAULT 'published',
                created_at TEXT DEFAULT (datetime('now')),
                updated_at TEXT DEFAULT (datetime('now'))
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
            CREATE TABLE posts_slides (
                id TEXT PRIMARY KEY,
                parent_id TEXT,
                _order INTEGER,
                caption TEXT,
                secret TEXT
            );
            INSERT INTO posts (id) VALUES ('p1');",
        )
        .unwrap();

        let no_locale = LocaleConfig::default();
        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            FieldDefinition::builder("slides", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("caption", FieldType::Text).build(),
                    FieldDefinition::builder("secret", FieldType::Text).build(),
                ])
                .build(),
        ];
        def.versions = Some(VersionsConfig::new(true, 10));

        // Live state: one slide with a secret only the privileged writer set.
        let sub = &def.fields[0].fields;
        set_array_rows(
            &conn,
            "posts",
            "slides",
            "p1",
            &[HashMap::from([
                ("caption".to_string(), json!("c1")),
                ("secret".to_string(), json!("live-secret")),
            ])],
            sub,
            None,
        )
        .unwrap();
        let row_id = find_array_rows(&conn, "posts", "slides", "p1", sub, None).unwrap()[0]["id"]
            .as_str()
            .unwrap()
            .to_string();

        // The snapshot as it reaches persistence AFTER the restore-time write
        // strip has removed the write-denied `secret`: the row keeps its `id`
        // (the strip never touches it) and changes `caption`.
        let stripped_snapshot = json!({ "slides": [ { "id": row_id, "caption": "c2" } ] });

        restore_version(
            &conn,
            "posts",
            &def,
            "p1",
            &stripped_snapshot,
            "published",
            &no_locale,
        )
        .unwrap();

        let after = find_array_rows(&conn, "posts", "slides", "p1", sub, None).unwrap();
        assert_eq!(
            after.len(),
            1,
            "the matched row is updated in place, not replaced"
        );
        assert_eq!(
            after[0]["id"].as_str().unwrap(),
            row_id,
            "row identity preserved"
        );
        assert_eq!(after[0]["caption"], "c2", "the restored field is written");
        assert_eq!(
            after[0]["secret"], "live-secret",
            "the write-denied field (omitted from the stripped snapshot) keeps its live value"
        );
    }

    #[test]
    fn restore_version_preserves_localized_timezone_companion() {
        // Regression: a LOCALIZED timezone Date's `_tz` companion columns are
        // per-locale (`start_date_tz__en`, `start_date_tz__de`) and go through
        // the locale-restore path, not `update()`. Restoring a version used to
        // rewrite only the date columns and leave the tz companions at their
        // current post-edit values — silently corrupting the restored timezone.
        let (_dir, conn) = setup_conn();
        conn.execute_batch(
            "CREATE TABLE events (
                id TEXT PRIMARY KEY,
                start_date__en TEXT,
                start_date__de TEXT,
                start_date_tz__en TEXT,
                start_date_tz__de TEXT,
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
            -- current (post-edit) state: both locales' timezone changed
            INSERT INTO events
                (id, start_date__en, start_date__de, start_date_tz__en, start_date_tz__de, _status)
                VALUES ('e1', '2024-06-15T14:00:00.000Z', '2024-06-15T14:00:00.000Z',
                        'Europe/London', 'Europe/London', 'published');",
        )
        .unwrap();

        let locale = LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: true,
        };

        let mut def = CollectionDefinition::new("events");
        def.fields = vec![
            FieldDefinition::builder("start_date", FieldType::Date)
                .timezone(true)
                .localized(true)
                .build(),
        ];
        def.versions = Some(VersionsConfig::new(true, 10));

        // Earlier version: en tz = New York, de tz = Berlin (default under the
        // bare key, non-default under `__de`, as the snapshot carries them).
        let snapshot_v1 = json!({
            "start_date": "2024-06-15T14:00:00.000Z",
            "start_date__de": "2024-06-15T14:00:00.000Z",
            "start_date_tz": "America/New_York",
            "start_date_tz__de": "Europe/Berlin",
        });
        create_version(&conn, "events", "e1", "published", &snapshot_v1).unwrap();

        restore_version(
            &conn,
            "events",
            &def,
            "e1",
            &snapshot_v1,
            "published",
            &locale,
        )
        .unwrap();

        let row = conn
            .query_one(
                "SELECT start_date_tz__en, start_date_tz__de FROM events WHERE id = 'e1'",
                &[],
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            row.get_string("start_date_tz__en").unwrap(),
            "America/New_York",
            "the default-locale _tz companion must be restored"
        );
        assert_eq!(
            row.get_string("start_date_tz__de").unwrap(),
            "Europe/Berlin",
            "the non-default-locale _tz companion must be restored"
        );
    }

    /// Regression: restoring a snapshot taken before email values were stored
    /// canonically wrote a localized address back as typed, so a lookup of the
    /// stored form missed it.
    #[test]
    fn restore_version_stores_a_localized_email_in_canonical_form() {
        let (_dir, conn) = setup_conn();
        conn.execute_batch(
            "CREATE TABLE people (
                id TEXT PRIMARY KEY,
                work__en TEXT,
                work__de TEXT,
                _status TEXT DEFAULT 'published',
                created_at TEXT DEFAULT (datetime('now')),
                updated_at TEXT DEFAULT (datetime('now'))
            );
            CREATE TABLE _versions_people (
                id TEXT PRIMARY KEY,
                _parent TEXT NOT NULL,
                _version INTEGER NOT NULL,
                _status TEXT NOT NULL,
                _latest INTEGER NOT NULL DEFAULT 0,
                snapshot TEXT NOT NULL,
                created_at TEXT DEFAULT (datetime('now')),
                updated_at TEXT DEFAULT (datetime('now'))
            );
            INSERT INTO people (id) VALUES ('p1');",
        )
        .unwrap();

        let locale = LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: true,
        };

        let mut def = CollectionDefinition::new("people");
        def.fields = vec![
            FieldDefinition::builder("work", FieldType::Email)
                .localized(true)
                .build(),
        ];
        def.versions = Some(VersionsConfig::new(true, 10));

        let snapshot = json!({
            "work": "Bob@Example.com",
            "work__de": "J\u{dc}RGEN@Example.com",
        });
        create_version(&conn, "people", "p1", "published", &snapshot).unwrap();

        restore_version(&conn, "people", &def, "p1", &snapshot, "published", &locale).unwrap();

        let row = conn
            .query_one("SELECT work__en, work__de FROM people WHERE id = 'p1'", &[])
            .unwrap()
            .unwrap();
        assert_eq!(row.get_string("work__en").unwrap(), "bob@example.com");
        assert_eq!(
            row.get_string("work__de").unwrap(),
            "j\u{fc}rgen@example.com"
        );
    }

    /// Regression: a snapshot recorded every per-locale column as text — a
    /// localized number or checkbox read back as a string, or not at all where
    /// the backend won't read a number as text — and restore cleared a
    /// localized scalar has-many list.
    #[test]
    fn restore_writes_typed_localized_values_back() {
        let (_dir, conn) = setup_conn();
        conn.execute_batch(
            "CREATE TABLE items (
                id TEXT PRIMARY KEY,
                price__en REAL,
                price__de REAL,
                done__en INTEGER,
                done__de INTEGER,
                tags__en TEXT,
                tags__de TEXT,
                _status TEXT DEFAULT 'published',
                created_at TEXT DEFAULT (datetime('now')),
                updated_at TEXT DEFAULT (datetime('now'))
            );
            CREATE TABLE _versions_items (
                id TEXT PRIMARY KEY,
                _parent TEXT NOT NULL,
                _version INTEGER NOT NULL,
                _status TEXT NOT NULL,
                _latest INTEGER NOT NULL DEFAULT 0,
                snapshot TEXT NOT NULL,
                created_at TEXT DEFAULT (datetime('now')),
                updated_at TEXT DEFAULT (datetime('now'))
            );
            INSERT INTO items (id, price__de, done__de, tags__de)
                VALUES ('i1', 12.5, 1, '[\"a\",\"b\"]');",
        )
        .unwrap();

        let locale = LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: true,
        };

        let mut def = CollectionDefinition::new("items");
        def.fields = vec![
            FieldDefinition::builder("price", FieldType::Number)
                .localized(true)
                .build(),
            FieldDefinition::builder("done", FieldType::Checkbox)
                .localized(true)
                .build(),
            FieldDefinition::builder("tags", FieldType::Select)
                .has_many(true)
                .localized(true)
                .build(),
        ];
        def.versions = Some(VersionsConfig::new(true, 10));

        let doc = Document::builder("i1").build();
        let snapshot = build_snapshot(&conn, "items", &def.fields, &doc, Some(&locale)).unwrap();
        assert_eq!(snapshot["price__de"], json!(12.5));
        assert_eq!(snapshot["done__de"], json!(1));
        assert_eq!(snapshot["tags__de"], json!(["a", "b"]));

        conn.execute(
            "UPDATE items SET price__de = NULL, done__de = 0, tags__de = NULL",
            &[],
        )
        .unwrap();
        restore_version(&conn, "items", &def, "i1", &snapshot, "published", &locale).unwrap();

        let row = conn
            .query_one(
                "SELECT price__de, done__de, tags__de FROM items WHERE id = 'i1'",
                &[],
            )
            .unwrap()
            .unwrap();
        assert!(matches!(row.get_value(0), Some(DbValue::Real(p)) if (p - 12.5).abs() < 1e-9));
        assert!(matches!(row.get_value(1), Some(DbValue::Integer(1))));
        assert_eq!(row.get_string("tags__de").unwrap(), r#"["a","b"]"#);
    }

    /// A hyphenated locale's snapshot keys carry the column form (`title__pt_BR`,
    /// `content__pt_BR`). Restore looked them up as `title__pt-BR`, found
    /// nothing, and cleared the translation and left its rows untouched.
    #[test]
    fn restore_reads_a_hyphenated_locales_snapshot_keys() {
        let (_dir, conn) = setup_conn();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                title__en TEXT,
                title__pt_BR TEXT,
                _status TEXT DEFAULT 'published',
                created_at TEXT DEFAULT (datetime('now')),
                updated_at TEXT DEFAULT (datetime('now'))
            );
            CREATE TABLE posts_content (
                id TEXT PRIMARY KEY,
                parent_id TEXT,
                _order INTEGER,
                _block_type TEXT,
                data TEXT,
                _locale TEXT
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
            INSERT INTO posts (id, title__en, title__pt_BR) VALUES ('p1', 'Now', 'Agora');",
        )
        .unwrap();

        let locale = LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "pt-BR".to_string()],
            fallback: true,
        };

        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text)
                .localized(true)
                .build(),
            FieldDefinition::builder("content", FieldType::Blocks)
                .localized(true)
                .build(),
        ];
        def.versions = Some(VersionsConfig::new(true, 10));

        let snapshot = json!({
            "title": "Then",
            "title__en": "Then",
            "title__pt_BR": "Antes",
            "content__en": [{ "_block_type": "hero", "heading": "Hi" }],
            "content__pt_BR": [{ "_block_type": "hero", "heading": "Oi" }],
        });

        restore_version(&conn, "posts", &def, "p1", &snapshot, "published", &locale).unwrap();

        let row = conn
            .query_one("SELECT title__pt_BR FROM posts WHERE id = 'p1'", &[])
            .unwrap()
            .unwrap();
        assert_eq!(row.get_string("title__pt_BR").unwrap(), "Antes");

        let rows = conn
            .query_one(
                "SELECT COUNT(*) AS cnt FROM posts_content WHERE _locale = 'pt-BR'",
                &[],
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            rows.get_i64("cnt").unwrap(),
            1,
            "the pt-BR rows are restored"
        );
    }

    /// The default locale's timezone comes from its OWN per-locale column. The
    /// bare `_tz` key holds whichever locale the snapshotted write was made
    /// under — German here — so preferring it put Berlin on the English date.
    #[test]
    fn restore_prefers_the_default_locales_own_timezone_column() {
        let (_dir, conn) = setup_conn();
        conn.execute_batch(
            "CREATE TABLE events (
                id TEXT PRIMARY KEY,
                start_date__en TEXT,
                start_date__de TEXT,
                start_date_tz__en TEXT,
                start_date_tz__de TEXT,
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
            INSERT INTO events
                (id, start_date__en, start_date__de, start_date_tz__en, start_date_tz__de, _status)
                VALUES ('e1', '2024-06-15T14:00:00.000Z', '2024-06-15T14:00:00.000Z',
                        'Europe/London', 'Europe/London', 'published');",
        )
        .unwrap();

        let locale = LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: true,
        };

        let mut def = CollectionDefinition::new("events");
        def.fields = vec![
            FieldDefinition::builder("start_date", FieldType::Date)
                .timezone(true)
                .localized(true)
                .build(),
        ];
        def.versions = Some(VersionsConfig::new(true, 10));

        // A version written under `de`: the bare key carries German's timezone,
        // while each locale's own column carries its real value.
        let snapshot = json!({
            "start_date": "2024-06-15T14:00:00.000Z",
            "start_date__en": "2024-06-15T14:00:00.000Z",
            "start_date__de": "2024-06-15T14:00:00.000Z",
            "start_date_tz": "Europe/Berlin",
            "start_date_tz__en": "America/New_York",
            "start_date_tz__de": "Europe/Berlin",
        });
        create_version(&conn, "events", "e1", "published", &snapshot).unwrap();

        restore_version(&conn, "events", &def, "e1", &snapshot, "published", &locale).unwrap();

        let row = conn
            .query_one(
                "SELECT start_date_tz__en, start_date_tz__de FROM events WHERE id = 'e1'",
                &[],
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            row.get_string("start_date_tz__en").unwrap(),
            "America/New_York",
            "the English date must keep its own timezone, not the German write's"
        );
        assert_eq!(
            row.get_string("start_date_tz__de").unwrap(),
            "Europe/Berlin"
        );
    }
}
