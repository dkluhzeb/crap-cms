//! Looking up a localized value in a snapshot, per locale.

use anyhow::Result;
use serde_json::{Map, Value};

use crate::{config::LocaleConfig, db::query::helpers::locale_column};

/// Resolve a snapshot value by trying the flat `"group__sub"` key first,
/// then navigating into the nested JSON object using the prefix segments.
pub(super) fn resolve_snapshot_value<'a>(
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
pub(crate) type SnapshotKey<'k> = (&'k str, &'k str, &'k str);

/// A snapshot's per-locale values.
///
/// The ONE resolver for "what does this snapshot hold for that locale", shared
/// by everything that writes a snapshot back over a row and by the validation
/// that judges what such a write will land — so the gate and the write can
/// never disagree about which locales a snapshot carries.
pub(crate) struct LocaleSnapshot<'a> {
    obj: &'a Map<String, Value>,
    pub(crate) config: &'a LocaleConfig,
}

impl<'a> LocaleSnapshot<'a> {
    pub(crate) fn new(obj: &'a Map<String, Value>, config: &'a LocaleConfig) -> Self {
        Self { obj, config }
    }

    /// The join rows (array / blocks / has-many) of `key` for `locale`:
    /// strictly the decorated `{key}__{locale}` entry, with no bare-key
    /// fallback. A snapshot records every configured locale's rows under its
    /// own key, so one that carries none for a locale predates that recording
    /// and the live rows are left alone — the bare key holds whichever locale
    /// the snapshotted write was made under, and taking it would copy those
    /// rows into another locale.
    ///
    /// The entry of a join field inside a group is found flat at the snapshot
    /// root (`seo__items__de`) or inside the group object (`seo.items__de`),
    /// the two places a value resolves from: a snapshot normalized to nested
    /// groups carries it in the group.
    pub(crate) fn rows(&self, key: &str, locale: &str) -> Result<Option<&'a Value>> {
        let decorated = locale_column(key, locale)?;

        // Field names never contain `__`, so the last one separates the group
        // path from the field.
        let (prefix, field) = key.rsplit_once("__").unwrap_or(("", key));
        let decorated_field = locale_column(field, locale)?;

        Ok(resolve_snapshot_value(
            self.obj,
            &decorated,
            prefix,
            &decorated_field,
        ))
    }

    /// The value of `key` for `locale`. EVERY locale prefers the decorated key
    /// the snapshot carries — `{key}__{locale}`, the locale code in column
    /// form. The bare key is only the default locale's fallback, for snapshots
    /// written before snapshots recorded every locale: it holds whichever
    /// locale the write that produced it was made under, so preferring it would
    /// copy (say) a German edit into the English column on restore.
    pub(crate) fn value(
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
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::LocaleSnapshot;
    use crate::{
        config::LocaleConfig,
        core::{CollectionDefinition, Document, FieldDefinition, FieldType, VersionsConfig},
        db::{
            DbConnection, DbValue,
            query::versions::{
                build_snapshot, create_version, restore::test_support::setup_conn, restore_version,
            },
        },
    };

    /// Regression: a snapshot whose groups were nested — as the write-access
    /// strip of a restore or a publish leaves it — carried a group's
    /// per-locale rows inside the group object, where the lookup never
    /// looked: that locale's rows were neither restored nor published.
    #[test]
    fn a_groups_per_locale_rows_are_found_flat_or_nested() {
        let config = LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: true,
        };
        let obj = json!({
            "seo": { "items__de": [{ "id": "nested" }] },
            "meta__links__de": [{ "id": "flat" }],
            "top__de": [{ "id": "top" }],
        });
        let snapshot = LocaleSnapshot::new(obj.as_object().unwrap(), &config);

        assert_eq!(
            snapshot.rows("seo__items", "de").unwrap(),
            Some(&json!([{ "id": "nested" }]))
        );
        assert_eq!(
            snapshot.rows("meta__links", "de").unwrap(),
            Some(&json!([{ "id": "flat" }]))
        );
        assert_eq!(
            snapshot.rows("top", "de").unwrap(),
            Some(&json!([{ "id": "top" }]))
        );
        assert_eq!(snapshot.rows("seo__items", "en").unwrap(), None);
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
                _revision INTEGER NOT NULL DEFAULT 0,
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
        assert_eq!(snapshot["done__de"], json!(true));
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
                _revision INTEGER NOT NULL DEFAULT 0,
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
                _revision INTEGER NOT NULL DEFAULT 0,
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
