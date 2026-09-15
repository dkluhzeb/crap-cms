//! Restoring a row's join-table rows (arrays, blocks, relationships) from a
//! snapshot, per locale for localized join fields.

use anyhow::Result;
use serde_json::{Map, Value};

use crate::{
    config::LocaleConfig,
    core::{DocumentFields, flatten_group_fields},
    db::query::{
        LocaleContext, LocaleMode,
        helpers::locale_column,
        join::save_join_table_data,
        versions::{
            localized_join_keys, restore::row::RestoreRow,
            snapshot::collect_join_data_from_snapshot,
        },
    },
};

/// Restore join table data from the snapshot. Localized join fields are left
/// to the per-locale pass: written here without a locale, every locale's rows
/// would land in the default locale.
pub(super) fn restore_join_rows(
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;

    use crate::{
        config::LocaleConfig,
        core::{CollectionDefinition, FieldDefinition, FieldTab, FieldType, VersionsConfig},
        db::{
            DbConnection, DbValue,
            query::{
                join::{find_array_rows, set_array_rows},
                versions::{count_versions, restore::test_support::setup_conn, restore_version},
            },
        },
    };

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
}
