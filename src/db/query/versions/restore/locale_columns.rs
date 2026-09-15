//! Restoring a row's localized columns (and their companions) from a snapshot.

use anyhow::{Context as _, Result};
use serde_json::{Map, Value};

use crate::{
    config::LocaleConfig,
    core::FieldDefinition,
    db::{
        DbConnection, DbValue,
        query::{
            helpers::{
                column_value, companion_value, locale_column, prefixed_name, quote_ident,
                walk_leaf_fields,
            },
            versions::restore::{
                locale_snapshot::{LocaleSnapshot, SnapshotKey},
                row::RestoreRow,
            },
        },
    },
};

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

/// Write every localized column back from the snapshot in one UPDATE.
pub(super) fn restore_locale_values(
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

            // A snapshot taken before email and text were stored canonically holds
            // the value as typed; the restore writes the stored form. Stored
            // dates are already normalized, so no zone is applied again.
            restore_locale_columns(set, &snapshot, key, |v| column_value(field, v, None))?;

            // Each companion (`{base}_tz`, `{base}_lang`) is localized the same
            // way and present in the snapshot (the locale SELECT emits it).
            // Restore its per-locale values too, with the same lookup order —
            // otherwise restoring an old version leaves the companion at its
            // current post-edit value.
            let companions = field
                .companion_columns(&base)
                .zip(field.companion_columns(&field.name));

            for (column, name) in companions {
                let companion_key = (column.as_str(), prefix, name.as_str());

                restore_locale_columns(set, &snapshot, companion_key, |v| {
                    companion_value(Some(v))
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::{
        config::LocaleConfig,
        core::{CollectionDefinition, FieldDefinition, FieldType, VersionsConfig},
        db::{
            DbConnection,
            query::versions::{
                create_version,
                restore::test_support::{VERSIONS_SNIPPETS_DDL, code_lang_def, en_de, setup_conn},
                restore_version,
            },
        },
    };

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

    /// Regression: restore wrote a localized code field's value columns back
    /// but left every locale's `_lang` companion at its post-edit value.
    #[test]
    fn restore_version_preserves_localized_code_language_companion() {
        let (_dir, conn) = setup_conn();
        conn.execute_batch(&format!(
            "CREATE TABLE snippets (
                id TEXT PRIMARY KEY,
                snippet__en TEXT,
                snippet__de TEXT,
                snippet_lang__en TEXT,
                snippet_lang__de TEXT,
                _status TEXT DEFAULT 'published',
                created_at TEXT DEFAULT (datetime('now')),
                updated_at TEXT DEFAULT (datetime('now'))
            );
            {VERSIONS_SNIPPETS_DDL}
            INSERT INTO snippets
                (id, snippet__en, snippet__de, snippet_lang__en, snippet_lang__de, _status)
                VALUES ('s1', 'x', 'y', 'python', 'python', 'published');"
        ))
        .unwrap();

        let def = code_lang_def(true);

        let snapshot_v1 = json!({
            "snippet": "console.log(1)",
            "snippet__en": "console.log(1)",
            "snippet__de": "print(1)",
            "snippet_lang": "javascript",
            "snippet_lang__en": "javascript",
            "snippet_lang__de": "python",
        });

        restore_version(
            &conn,
            "snippets",
            &def,
            "s1",
            &snapshot_v1,
            "published",
            &en_de(),
        )
        .unwrap();

        let row = conn
            .query_one(
                "SELECT snippet_lang__en, snippet_lang__de FROM snippets WHERE id = 's1'",
                &[],
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            row.get_string("snippet_lang__en").unwrap(),
            "javascript",
            "the default-locale _lang companion must be restored"
        );
        assert_eq!(row.get_string("snippet_lang__de").unwrap(), "python");
    }
}
