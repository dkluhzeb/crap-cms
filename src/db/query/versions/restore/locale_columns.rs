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

            // A snapshot taken before email and text were stored canonically holds
            // the value as typed; the restore writes the stored form. Stored
            // dates are already normalized, so no zone is applied again.
            //
            // A field the snapshot carries no value for in ANY locale — no bare
            // key and no locale column — was either removed from the restore
            // (the caller is write-denied on it) or did not exist when the
            // snapshot was taken, so nothing is emitted and its stored columns
            // are left alone, exactly as a missing non-localized field is.
            let restored =
                restore_value_locales(set, &snapshot, key, |v| column_value(field, v, None))?;

            restore_companions(set, &snapshot, &LocalizedLeaf { field, key }, &restored)
        },
    )
}

/// A localized leaf as the restore reaches it: where its value sits in the
/// snapshot, and the definition that decides which companions a write stores
/// beside it.
struct LocalizedLeaf<'a> {
    field: &'a FieldDefinition,
    key: SnapshotKey<'a>,
}

/// Emit SET clauses restoring every locale column of a field's value from the
/// snapshot, and report the locales they cover. Restoring used to NULL every
/// non-default locale even though snapshots carry the decorated `__xx` values —
/// wiping translations.
///
/// A locale the snapshot has NO key for at all was not configured when the
/// snapshot was taken, so it is left untouched instead of erasing a translation
/// the version never knew about. A key that IS present and holds `null` still
/// clears the column: a snapshot records every configured locale's column,
/// `null` included, so a locale that was empty at snapshot time is restored
/// empty.
fn restore_value_locales<'s>(
    set: &mut SetClauses<'_>,
    snapshot: &LocaleSnapshot<'s>,
    key: SnapshotKey<'_>,
    column_value: impl Fn(&Value) -> DbValue,
) -> Result<Vec<&'s str>> {
    let mut restored = Vec::new();
    let config = snapshot.config;

    for locale in &config.locales {
        let Some(value) = snapshot.value(key, locale)? else {
            continue;
        };

        let col = locale_column(key.0, locale)?;
        let db_val = Some(column_value(value)).filter(|v| !v.is_null());

        set.push(&col, db_val);
        restored.push(locale.as_str());
    }

    Ok(restored)
}

/// Emit SET clauses restoring a field's companion columns (`{base}_tz`,
/// `{base}_lang`) for each locale whose VALUE the restore writes — each
/// companion is localized the same way and present in the snapshot (the locale
/// SELECT emits it), so restoring an old version would otherwise leave it at
/// its current post-edit value.
///
/// Which companions a write stores is decided by
/// `written_companion_columns`, and a restore is a write: a zone travels with
/// its date, so a snapshot taken before the date gained one clears today's zone
/// instead of pinning it on a rolled-back value; a language the snapshot does
/// not name keeps the stored pick, the way an update that sends no language
/// does. A locale whose value is left untouched keeps its companion untouched
/// too.
fn restore_companions(
    set: &mut SetClauses<'_>,
    snapshot: &LocaleSnapshot<'_>,
    leaf: &LocalizedLeaf<'_>,
    locales: &[&str],
) -> Result<()> {
    let LocalizedLeaf { field, key } = *leaf;
    let (base, prefix, name) = key;

    let with_value: Vec<String> = field
        .written_companion_columns(base, true, |_| false)
        .collect();
    let companions = field
        .companion_columns(base)
        .zip(field.companion_columns(name));

    for (column, companion_name) in companions {
        let companion_key = (column.as_str(), prefix, companion_name.as_str());
        let travels_with_value = with_value.contains(&column);

        for locale in locales {
            let stored = snapshot.value(companion_key, locale)?;

            if stored.is_none() && !travels_with_value {
                continue;
            }

            let col = locale_column(column.as_str(), locale)?;
            let db_val = stored
                .map(|value| companion_value(Some(value)))
                .filter(|v| !v.is_null());

            set.push(&col, db_val);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

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

    /// A locale added AFTER a version was taken has no key in that snapshot.
    /// Restoring wrote NULL for it and wiped the translation — the opposite of
    /// the rule a field the snapshot doesn't carry at all follows. A key that
    /// IS present and holds `null` still clears its column.
    #[test]
    fn restore_leaves_a_locale_the_snapshot_predates_untouched() {
        let (_dir, conn) = setup_conn();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                title__en TEXT,
                title__de TEXT,
                title__fr TEXT,
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
            INSERT INTO posts (id, title__en, title__de, title__fr)
                VALUES ('p1', 'Now', 'Jetzt', 'Maintenant');",
        )
        .unwrap();

        // `fr` was added after this snapshot; `de` was configured then and
        // held nothing, so the snapshot records it as an explicit null.
        let locale = LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string(), "fr".to_string()],
            fallback: true,
        };

        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text)
                .localized(true)
                .build(),
        ];
        def.versions = Some(VersionsConfig::new(true, 10));

        let snapshot = json!({
            "title": "Then",
            "title__en": "Then",
            "title__de": Value::Null,
        });

        restore_version(&conn, "posts", &def, "p1", &snapshot, "published", &locale).unwrap();

        let row = conn
            .query_one(
                "SELECT title__en, title__de, title__fr FROM posts WHERE id = 'p1'",
                &[],
            )
            .unwrap()
            .unwrap();
        assert_eq!(row.get_string("title__en").unwrap(), "Then");
        assert!(
            row.opt_text_at(1).is_none(),
            "a locale the snapshot records as null is cleared"
        );
        assert_eq!(
            row.get_string("title__fr").unwrap(),
            "Maintenant",
            "a locale the snapshot predates must be left untouched"
        );
    }

    /// Regression: the per-locale skip (a locale the snapshot has no key for is
    /// left untouched) also skipped an absent COMPANION of a value the snapshot
    /// DOES carry, so restoring a snapshot taken before the date gained
    /// `timezone = true` rolled the date back and kept today's zone on it. A
    /// zone travels with its date: it is cleared where the snapshot carries
    /// none.
    #[test]
    fn restore_clears_a_timezone_the_snapshot_predates() {
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
                (id, start_date__en, start_date__de, start_date_tz__en, start_date_tz__de)
                VALUES ('e1', '2025-01-01T10:00:00.000Z', '2025-01-01T10:00:00.000Z',
                        'Europe/London', 'Europe/London');",
        )
        .unwrap();

        let mut def = CollectionDefinition::new("events");
        def.fields = vec![
            FieldDefinition::builder("start_date", FieldType::Date)
                .timezone(true)
                .localized(true)
                .build(),
        ];
        def.versions = Some(VersionsConfig::new(true, 10));

        // Taken while the field stored no zone at all: the values are there,
        // the companions are not.
        let snapshot = json!({
            "start_date": "2024-06-15T14:00:00.000Z",
            "start_date__en": "2024-06-15T14:00:00.000Z",
            "start_date__de": "2024-06-15T14:00:00.000Z",
        });

        restore_version(
            &conn,
            "events",
            &def,
            "e1",
            &snapshot,
            "published",
            &en_de(),
        )
        .unwrap();

        let row = conn
            .query_one(
                "SELECT start_date_tz__en, start_date_tz__de FROM events WHERE id = 'e1'",
                &[],
            )
            .unwrap()
            .unwrap();
        assert!(
            row.opt_text_at(0).is_none() && row.opt_text_at(1).is_none(),
            "a restored date must not keep the zone it was given after the snapshot"
        );
    }

    /// The other half of the rule a write follows: a language the snapshot does
    /// not name keeps the stored pick, the way an update that sends no language
    /// does — only a companion that travels with its value is cleared.
    #[test]
    fn restore_keeps_a_language_the_snapshot_does_not_name() {
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
                (id, snippet__en, snippet__de, snippet_lang__en, snippet_lang__de)
                VALUES ('s1', 'x', 'y', 'python', 'python');"
        ))
        .unwrap();

        let snapshot = json!({
            "snippet": "console.log(1)",
            "snippet__en": "console.log(1)",
            "snippet__de": "print(1)",
        });

        restore_version(
            &conn,
            "snippets",
            &code_lang_def(true),
            "s1",
            &snapshot,
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
        assert_eq!(row.get_string("snippet_lang__en").unwrap(), "python");
        assert_eq!(row.get_string("snippet_lang__de").unwrap(), "python");
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
