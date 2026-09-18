//! Restoring a version snapshot onto a collection document.

use anyhow::{Result, anyhow};
use serde_json::{Map, Value};

use crate::{
    config::LocaleConfig,
    core::{CollectionDefinition, Document, collection::VersionsConfig},
    db::{
        DbConnection,
        query::{
            LocaleContext,
            fts::fts_upsert,
            read::find_by_id_raw,
            ref_count,
            versions::{
                VersionWrite, create_version_and_prune, restore::row::restore_locale_and_join_data,
                set_document_status, snapshot::extract_snapshot_data,
            },
            write::update,
        },
    },
};

/// Write a stored snapshot back over a document's row: the regular columns,
/// every locale's own column, and the join rows.
///
/// The shared write half of restoring a version and of publishing a pending
/// draft — a publish takes the draft as ONE unit, so every locale the
/// publishing request does not target takes its values from the snapshot
/// exactly as a restore of that snapshot would write them.
///
/// Deliberately does not touch `_status`, version history, ref counts or the
/// search index: the two lifecycles account for those differently, and a
/// publish has to fold this write into its own ref-count bracket.
///
/// # Errors
///
/// Returns a backend error if the UPDATE or the locale/join-table sync fails.
pub fn write_snapshot_base(
    conn: &dyn DbConnection,
    slug: &str,
    def: &CollectionDefinition,
    parent_id: &str,
    obj: &Map<String, Value>,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let data = extract_snapshot_data(obj, &def.fields, locale_config.is_enabled());
    let locale_ctx = LocaleContext::default_for(locale_config);

    update(conn, slug, def, parent_id, &data, locale_ctx.as_ref())?;

    restore_locale_and_join_data(conn, slug, parent_id, &def.fields, obj, locale_config)
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

    let locale_ctx = LocaleContext::default_for(locale_config);

    // Row lock before the unlocked ref snapshot (see `persist_update`).
    conn.lock_row(slug, parent_id)?;
    let old_refs =
        ref_count::snapshot_outgoing_refs(conn, slug, parent_id, &def.fields, locale_config)?;

    write_snapshot_base(conn, slug, def, parent_id, obj, locale_config)?;

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

    // Recording the restore is a version write like any other, so it prunes to
    // the same cap: restoring repeatedly used to grow the table without bound.
    create_version_and_prune(
        conn,
        &VersionWrite::builder(slug, parent_id, status, snapshot)
            .max_versions(VersionsConfig::cap(def.versions.as_ref()))
            .build(),
    )?;

    // The restored document as it now stands — locale columns, rows and status
    // written — not the row as the column update read it mid-restore.
    find_by_id_raw(conn, slug, def, parent_id, locale_ctx.as_ref(), false)?
        .ok_or_else(|| anyhow!("Document {parent_id} not found after restore"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::restore_version;
    use crate::{
        config::LocaleConfig,
        core::{CollectionDefinition, FieldDefinition, FieldType, VersionsConfig},
        db::{
            DbConnection,
            query::{
                find_by_id,
                versions::{
                    build_snapshot, count_versions, create_version,
                    restore::test_support::{VERSIONS_SNIPPETS_DDL, code_lang_def, setup_conn},
                },
            },
        },
    };

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

    /// Restoring records a version like every other lifecycle step, so it
    /// prunes to the same cap. Restore created a version and never pruned, so
    /// repeated restores grew the version table without bound.
    #[test]
    fn restore_version_prunes_to_the_configured_cap() {
        let (_dir, conn) = setup_conn();
        conn.execute_batch(
            "CREATE TABLE notes (
                id TEXT PRIMARY KEY,
                body TEXT,
                created_at TEXT DEFAULT (datetime('now')),
                updated_at TEXT DEFAULT (datetime('now'))
            );
            CREATE TABLE _versions_notes (
                id TEXT PRIMARY KEY,
                _parent TEXT NOT NULL,
                _version INTEGER NOT NULL,
                _status TEXT NOT NULL,
                _latest INTEGER NOT NULL DEFAULT 0,
                snapshot TEXT NOT NULL,
                created_at TEXT DEFAULT (datetime('now')),
                updated_at TEXT DEFAULT (datetime('now'))
            );
            INSERT INTO notes (id, body) VALUES ('n1', 'live');",
        )
        .unwrap();

        let no_locale = LocaleConfig::default();
        let mut def = CollectionDefinition::new("notes");
        def.fields = vec![FieldDefinition::builder("body", FieldType::Text).build()];
        def.versions = Some(VersionsConfig::new(false, 2));

        let snapshot = json!({ "body": "v1" });

        for _ in 0..5 {
            restore_version(
                &conn,
                "notes",
                &def,
                "n1",
                &snapshot,
                "published",
                &no_locale,
            )
            .unwrap();
        }

        assert_eq!(
            count_versions(&conn, "notes", "n1", false).unwrap(),
            2,
            "five restores must leave the cap, not five rows"
        );
    }

    /// Regression: a version snapshot never carried a code field's `_lang`
    /// companion and restore never wrote it back, so restoring a version kept
    /// the current language instead of the snapshotted one.
    #[test]
    fn restore_version_round_trips_code_language() {
        let (_dir, conn) = setup_conn();
        conn.execute_batch(&format!(
            "CREATE TABLE snippets (
                id TEXT PRIMARY KEY,
                snippet TEXT,
                snippet_lang TEXT,
                _status TEXT DEFAULT 'published',
                created_at TEXT DEFAULT (datetime('now')),
                updated_at TEXT DEFAULT (datetime('now'))
            );
            {VERSIONS_SNIPPETS_DDL}
            INSERT INTO snippets (id, snippet, snippet_lang) VALUES ('s1', 'print(1)', 'python');"
        ))
        .unwrap();

        let no_locale = LocaleConfig::default();
        let def = code_lang_def(false);

        let doc = find_by_id(&conn, "snippets", &def, "s1", None)
            .unwrap()
            .unwrap();
        let snapshot = build_snapshot(&conn, "snippets", &def.fields, &doc, None).unwrap();
        assert_eq!(
            snapshot["snippet_lang"],
            json!("python"),
            "the snapshot must record the language companion"
        );

        conn.execute_batch(
            "UPDATE snippets SET snippet = 'fn main() {}', snippet_lang = 'javascript' WHERE id = 's1'",
        )
        .unwrap();

        let restored = restore_version(
            &conn,
            "snippets",
            &def,
            "s1",
            &snapshot,
            "published",
            &no_locale,
        )
        .unwrap();

        let row = conn
            .query_one("SELECT snippet_lang FROM snippets WHERE id = 's1'", &[])
            .unwrap()
            .unwrap();
        assert_eq!(row.get_string("snippet_lang").unwrap(), "python");
        assert_eq!(restored.get_str("snippet_lang"), Some("python"));
    }
}
