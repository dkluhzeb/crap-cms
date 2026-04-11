//! Locale resolution helpers for join table hydration.

use crate::{
    core::FieldDefinition,
    db::{LocaleContext, LocaleMode},
};

/// Resolve the effective locale string for a join table operation.
/// Returns Some("en") when the field is localized and locale is enabled,
/// None otherwise (same pattern as locale_write_column for regular columns).
pub(super) fn resolve_join_locale(
    field: &FieldDefinition,
    locale_ctx: Option<&LocaleContext>,
) -> Option<String> {
    let ctx = locale_ctx?;

    if !field.localized || !ctx.config.is_enabled() {
        return None;
    }

    let locale = match &ctx.mode {
        LocaleMode::Single(l) => l.as_str(),
        _ => ctx.config.default_locale.as_str(),
    };

    Some(locale.to_string())
}

/// When fallback is enabled and we're querying a non-default locale,
/// returns the default locale to fall back to if the primary query returns empty.
pub(super) fn resolve_join_fallback_locale(
    field: &FieldDefinition,
    locale_ctx: Option<&LocaleContext>,
) -> Option<String> {
    let ctx = locale_ctx?;

    if !field.localized || !ctx.config.is_enabled() || !ctx.config.fallback {
        return None;
    }

    match &ctx.mode {
        LocaleMode::Single(l) if l != &ctx.config.default_locale => {
            Some(ctx.config.default_locale.clone())
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{
        config::LocaleConfig,
        core::field::{FieldDefinition, FieldType, RelationshipConfig},
        db::query::{LocaleContext, LocaleMode},
    };
    use rusqlite::Connection;

    use super::super::super::relationships::{find_related_ids, set_related_ids};
    use super::super::hydrate_document;

    fn localized_tags_field() -> FieldDefinition {
        FieldDefinition::builder("tags", FieldType::Relationship)
            .localized(true)
            .relationship(RelationshipConfig::new("tags", true))
            .build()
    }

    fn de_fallback_ctx() -> LocaleContext {
        LocaleContext {
            mode: LocaleMode::Single("de".to_string()),
            config: LocaleConfig {
                default_locale: "en".to_string(),
                locales: vec!["en".to_string(), "de".to_string()],
                fallback: true,
            },
        }
    }

    #[test]
    fn hydrate_fallback_locale_for_has_many() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (id TEXT PRIMARY KEY);
             CREATE TABLE posts_tags (
                 parent_id TEXT,
                 related_id TEXT,
                 _order INTEGER,
                 _locale TEXT
             );
             INSERT INTO posts (id) VALUES ('p1');
             -- Only 'en' locale data exists, no 'de'
             INSERT INTO posts_tags (parent_id, related_id, _order, _locale) VALUES ('p1', 't1', 0, 'en');",
        ).unwrap();

        let locale_ctx = de_fallback_ctx();

        let mut doc = crate::core::Document::new("p1".to_string());
        hydrate_document(
            &conn,
            "posts",
            &[localized_tags_field()],
            &mut doc,
            None,
            Some(&locale_ctx),
        )
        .unwrap();

        let tags = doc
            .fields
            .get("tags")
            .expect("tags should be hydrated via fallback");
        let arr = tags.as_array().expect("should be array");
        assert_eq!(arr.len(), 1, "should fall back to 'en' when 'de' is empty");
        assert_eq!(arr[0].as_str(), Some("t1"));
    }

    #[test]
    fn hydrate_fallback_locale_for_arrays() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (id TEXT PRIMARY KEY);
             CREATE TABLE posts_items (
                 id TEXT PRIMARY KEY,
                 parent_id TEXT,
                 _order INTEGER,
                 label TEXT,
                 _locale TEXT
             );
             INSERT INTO posts (id) VALUES ('p1');
             INSERT INTO posts_items (id, parent_id, _order, label, _locale) VALUES ('i1', 'p1', 0, 'EN Item', 'en');",
        ).unwrap();

        let items_field = FieldDefinition::builder("items", FieldType::Array)
            .localized(true)
            .fields(vec![
                FieldDefinition::builder("label", FieldType::Text).build(),
            ])
            .build();

        let locale_ctx = de_fallback_ctx();

        let mut doc = crate::core::Document::new("p1".to_string());
        hydrate_document(
            &conn,
            "posts",
            &[items_field],
            &mut doc,
            None,
            Some(&locale_ctx),
        )
        .unwrap();

        let items = doc
            .fields
            .get("items")
            .expect("items should be hydrated via fallback");
        let arr = items.as_array().expect("should be array");
        assert_eq!(
            arr.len(),
            1,
            "should fall back to 'en' items when 'de' is empty"
        );
        assert_eq!(arr[0]["label"], "EN Item");
    }

    #[test]
    fn hydrate_fallback_locale_for_blocks() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (id TEXT PRIMARY KEY);
             CREATE TABLE posts_content (
                 id TEXT PRIMARY KEY,
                 parent_id TEXT,
                 _order INTEGER,
                 _block_type TEXT,
                 data TEXT,
                 _locale TEXT
             );
             INSERT INTO posts (id) VALUES ('p1');
             INSERT INTO posts_content (id, parent_id, _order, _block_type, data, _locale)
                 VALUES ('b1', 'p1', 0, 'text', '{\"body\":\"EN Content\"}', 'en');",
        )
        .unwrap();

        let content_field = FieldDefinition::builder("content", FieldType::Blocks)
            .localized(true)
            .build();

        let locale_ctx = de_fallback_ctx();

        let mut doc = crate::core::Document::new("p1".to_string());
        hydrate_document(
            &conn,
            "posts",
            &[content_field],
            &mut doc,
            None,
            Some(&locale_ctx),
        )
        .unwrap();

        let content = doc
            .fields
            .get("content")
            .expect("content should be hydrated via fallback");
        let arr = content.as_array().expect("should be array");
        assert_eq!(
            arr.len(),
            1,
            "should fall back to 'en' blocks when 'de' is empty"
        );
        assert_eq!(arr[0]["_block_type"], "text");
    }

    #[test]
    fn hydrate_fallback_not_triggered_when_data_exists() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (id TEXT PRIMARY KEY);
             CREATE TABLE posts_tags (
                 parent_id TEXT,
                 related_id TEXT,
                 _order INTEGER,
                 _locale TEXT
             );
             INSERT INTO posts (id) VALUES ('p1');
             INSERT INTO posts_tags (parent_id, related_id, _order, _locale) VALUES ('p1', 'de_tag1', 0, 'de');
             INSERT INTO posts_tags (parent_id, related_id, _order, _locale) VALUES ('p1', 'en_tag1', 0, 'en');",
        ).unwrap();

        let locale_ctx = de_fallback_ctx();

        let mut doc = crate::core::Document::new("p1".to_string());
        hydrate_document(
            &conn,
            "posts",
            &[localized_tags_field()],
            &mut doc,
            None,
            Some(&locale_ctx),
        )
        .unwrap();

        let tags = doc.fields.get("tags").unwrap();
        let arr = tags.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(
            arr[0].as_str(),
            Some("de_tag1"),
            "should use de data, not fall back to en"
        );
    }

    #[test]
    fn hydrate_fallback_locale_for_polymorphic_has_many() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (id TEXT PRIMARY KEY);
             CREATE TABLE posts_refs (
                 parent_id TEXT,
                 related_id TEXT,
                 related_collection TEXT NOT NULL DEFAULT '',
                 _order INTEGER,
                 _locale TEXT
             );
             INSERT INTO posts (id) VALUES ('p1');
             INSERT INTO posts_refs (parent_id, related_id, related_collection, _order, _locale)
                 VALUES ('p1', 'a1', 'articles', 0, 'en');",
        )
        .unwrap();

        let mut refs_rel = RelationshipConfig::new("articles", true);
        refs_rel.polymorphic = vec!["articles".into(), "pages".into()];
        let refs_field = FieldDefinition::builder("refs", FieldType::Relationship)
            .localized(true)
            .relationship(refs_rel)
            .build();

        let locale_ctx = de_fallback_ctx();

        let mut doc = crate::core::Document::new("p1".to_string());
        hydrate_document(
            &conn,
            "posts",
            &[refs_field],
            &mut doc,
            None,
            Some(&locale_ctx),
        )
        .unwrap();

        let refs = doc
            .fields
            .get("refs")
            .expect("refs should be hydrated via fallback");
        let arr = refs.as_array().expect("should be array");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0].as_str(), Some("articles/a1"));
    }

    // ── Group > Array locale fallback ───────────────────────────────────

    #[test]
    fn hydrate_group_array_locale_fallback() {
        use super::super::super::arrays::set_array_rows;

        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (id TEXT PRIMARY KEY, config__label TEXT);
             CREATE TABLE posts_config__items (
                 id TEXT PRIMARY KEY,
                 parent_id TEXT,
                 _order INTEGER,
                 name TEXT,
                 _locale TEXT
             );
             INSERT INTO posts (id, config__label) VALUES ('p1', 'My Config');",
        )
        .unwrap();

        // Insert EN-only data
        let sub = vec![FieldDefinition::builder("name", FieldType::Text).build()];
        let rows = vec![std::collections::HashMap::from([(
            "name".to_string(),
            "FallbackItem".to_string(),
        )])];
        set_array_rows(
            &conn,
            "posts",
            "config__items",
            "p1",
            &rows,
            &sub,
            Some("en"),
        )
        .unwrap();

        let fields = vec![
            FieldDefinition::builder("config", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("label", FieldType::Text).build(),
                    FieldDefinition::builder("items", FieldType::Array)
                        .localized(true)
                        .fields(vec![
                            FieldDefinition::builder("name", FieldType::Text).build(),
                        ])
                        .build(),
                ])
                .build(),
        ];

        // Query in DE with fallback enabled — should get EN data
        let locale_ctx = de_fallback_ctx();
        let mut doc = crate::core::Document::new("p1".to_string());
        doc.fields
            .insert("config__label".to_string(), json!("My Config"));

        hydrate_document(&conn, "posts", &fields, &mut doc, None, Some(&locale_ctx)).unwrap();

        let config = doc
            .fields
            .get("config")
            .expect("config group should be hydrated");
        let items = config
            .get("items")
            .expect("items should exist via fallback");
        let items_arr = items.as_array().expect("items should be array");
        assert_eq!(items_arr.len(), 1, "should get EN items via fallback");
        assert_eq!(items_arr[0]["name"], "FallbackItem");
    }

    // ── resolve_join_locale unit tests ────────────────────────────────────

    #[test]
    fn resolve_join_locale_returns_none_when_no_ctx() {
        let field = FieldDefinition::builder("tags", FieldType::Relationship)
            .localized(true)
            .build();
        assert!(resolve_join_locale(&field, None).is_none());
    }

    #[test]
    fn resolve_join_locale_returns_none_when_not_localized() {
        let field = FieldDefinition::builder("tags", FieldType::Relationship)
            .localized(false)
            .build();
        let locale_ctx = de_fallback_ctx();
        assert!(resolve_join_locale(&field, Some(&locale_ctx)).is_none());
    }

    #[test]
    fn resolve_join_fallback_locale_returns_none_when_fallback_disabled() {
        let field = FieldDefinition::builder("tags", FieldType::Relationship)
            .localized(true)
            .build();
        let ctx = LocaleContext {
            mode: LocaleMode::Single("de".to_string()),
            config: LocaleConfig {
                default_locale: "en".to_string(),
                locales: vec!["en".to_string(), "de".to_string()],
                fallback: false,
            },
        };
        assert!(resolve_join_fallback_locale(&field, Some(&ctx)).is_none());
    }

    #[test]
    fn resolve_join_fallback_locale_returns_none_when_already_default() {
        let field = FieldDefinition::builder("tags", FieldType::Relationship)
            .localized(true)
            .build();
        let ctx = LocaleContext {
            mode: LocaleMode::Single("en".to_string()),
            config: LocaleConfig {
                default_locale: "en".to_string(),
                locales: vec!["en".to_string(), "de".to_string()],
                fallback: true,
            },
        };
        assert!(resolve_join_fallback_locale(&field, Some(&ctx)).is_none());
    }

    // ── resolve_join_locale with All/Multi modes ──────────────────────────

    #[test]
    fn resolve_join_locale_all_mode_uses_default() {
        let field = FieldDefinition::builder("tags", FieldType::Relationship)
            .localized(true)
            .build();
        let ctx = LocaleContext {
            mode: LocaleMode::All,
            config: LocaleConfig {
                default_locale: "en".to_string(),
                locales: vec!["en".to_string(), "de".to_string()],
                fallback: false,
            },
        };
        assert_eq!(
            resolve_join_locale(&field, Some(&ctx)),
            Some("en".to_string())
        );
    }

    // ── set_related_ids / find_related_ids locale integration ─────────────

    #[test]
    fn locale_scopes_related_ids_correctly() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (id TEXT PRIMARY KEY);
             CREATE TABLE posts_tags (
                 parent_id TEXT,
                 related_id TEXT,
                 _order INTEGER,
                 _locale TEXT
             );
             INSERT INTO posts (id) VALUES ('p1');",
        )
        .unwrap();

        set_related_ids(
            &conn,
            "posts",
            "tags",
            "p1",
            &["t1".to_string()],
            Some("en"),
        )
        .unwrap();
        set_related_ids(
            &conn,
            "posts",
            "tags",
            "p1",
            &["t2".to_string()],
            Some("de"),
        )
        .unwrap();

        let en_ids = find_related_ids(&conn, "posts", "tags", "p1", Some("en")).unwrap();
        let de_ids = find_related_ids(&conn, "posts", "tags", "p1", Some("de")).unwrap();

        assert_eq!(en_ids, vec!["t1"]);
        assert_eq!(de_ids, vec!["t2"]);
    }

    // ── hydrate_document with locale (no fallback) ────────────────────────

    #[test]
    fn hydrate_locale_single_mode_filters_correctly() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (id TEXT PRIMARY KEY);
             CREATE TABLE posts_tags (
                 parent_id TEXT,
                 related_id TEXT,
                 _order INTEGER,
                 _locale TEXT
             );
             INSERT INTO posts (id) VALUES ('p1');
             INSERT INTO posts_tags VALUES ('p1', 'fr_tag', 0, 'fr');
             INSERT INTO posts_tags VALUES ('p1', 'en_tag', 0, 'en');",
        )
        .unwrap();

        let field = FieldDefinition::builder("tags", FieldType::Relationship)
            .localized(true)
            .relationship(RelationshipConfig::new("tags", true))
            .build();

        let ctx = LocaleContext {
            mode: LocaleMode::Single("fr".to_string()),
            config: LocaleConfig {
                default_locale: "en".to_string(),
                locales: vec!["en".to_string(), "fr".to_string()],
                fallback: false,
            },
        };

        let mut doc = crate::core::Document::new("p1".to_string());
        hydrate_document(&conn, "posts", &[field], &mut doc, None, Some(&ctx)).unwrap();

        let tags = doc.fields.get("tags").unwrap();
        let arr = tags.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0].as_str(), Some("fr_tag"));
    }
}
