//! Document hydration and join table save orchestration.

mod save;

pub use save::save_join_table_data;

use anyhow::Result;

use super::super::{LocaleContext, LocaleMode};
use super::arrays::find_array_rows;
use super::blocks::find_block_rows;
use super::relationships::{find_polymorphic_related, find_related_ids};
use crate::core::Document;
use crate::core::field::{FieldDefinition, FieldType};

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

/// Recursively extract prefixed columns from `doc.fields` into a nested Group object.
/// Handles Group→Row, Group→Collapsible, Group→Tabs, and Group→Group nesting.
fn reconstruct_group_fields(
    fields: &[FieldDefinition],
    prefix: &str,
    doc: &mut Document,
    group_obj: &mut serde_json::Map<String, serde_json::Value>,
) {
    for sub in fields {
        match sub.field_type {
            FieldType::Group => {
                // Nested group: collect sub-group's fields into a nested object
                let new_prefix = format!("{}__{}", prefix, sub.name);
                let mut sub_obj = serde_json::Map::new();
                reconstruct_group_fields(&sub.fields, &new_prefix, doc, &mut sub_obj);
                if !sub_obj.is_empty() {
                    group_obj.insert(sub.name.clone(), serde_json::Value::Object(sub_obj));
                }
            }
            FieldType::Row | FieldType::Collapsible => {
                // Layout fields are transparent — promote sub-fields to same level
                reconstruct_group_fields(&sub.fields, prefix, doc, group_obj);
            }
            FieldType::Tabs => {
                for tab in &sub.tabs {
                    reconstruct_group_fields(&tab.fields, prefix, doc, group_obj);
                }
            }
            _ => {
                let col_name = format!("{}__{}", prefix, sub.name);
                if let Some(val) = doc.fields.remove(&col_name) {
                    group_obj.insert(sub.name.clone(), val);
                }
            }
        }
    }
}

/// Hydrate a document with join table data (has-many relationships and arrays).
/// Populates `doc.fields` with JSON arrays for each join-table field.
/// If `select` is provided, skip hydrating fields not in the select list.
/// When `locale_ctx` is provided, localized join fields are filtered by locale.
pub fn hydrate_document(
    conn: &rusqlite::Connection,
    slug: &str,
    fields: &[FieldDefinition],
    doc: &mut Document,
    select: Option<&[String]>,
    locale_ctx: Option<&LocaleContext>,
) -> Result<()> {
    for field in fields {
        // Skip hydrating fields not in the select list
        if let Some(sel) = select {
            if !sel.iter().any(|s| s == &field.name) {
                continue;
            }
        }
        let locale = resolve_join_locale(field, locale_ctx);
        let locale_ref = locale.as_deref();
        let fallback_locale = resolve_join_fallback_locale(field, locale_ctx);
        let fallback_ref = fallback_locale.as_deref();
        match field.field_type {
            FieldType::Relationship | FieldType::Upload => {
                if let Some(ref rc) = field.relationship {
                    if rc.has_many {
                        if rc.is_polymorphic() {
                            let mut items = find_polymorphic_related(
                                conn,
                                slug,
                                &field.name,
                                &doc.id,
                                locale_ref,
                            )?;
                            if items.is_empty() && fallback_ref.is_some() {
                                items = find_polymorphic_related(
                                    conn,
                                    slug,
                                    &field.name,
                                    &doc.id,
                                    fallback_ref,
                                )?;
                            }
                            let json_items: Vec<serde_json::Value> = items
                                .into_iter()
                                .map(|(col, id)| {
                                    serde_json::Value::String(format!("{}/{}", col, id))
                                })
                                .collect();
                            doc.fields
                                .insert(field.name.clone(), serde_json::Value::Array(json_items));
                        } else {
                            let mut ids =
                                find_related_ids(conn, slug, &field.name, &doc.id, locale_ref)?;
                            if ids.is_empty() && fallback_ref.is_some() {
                                ids = find_related_ids(
                                    conn,
                                    slug,
                                    &field.name,
                                    &doc.id,
                                    fallback_ref,
                                )?;
                            }
                            let json_ids: Vec<serde_json::Value> =
                                ids.into_iter().map(serde_json::Value::String).collect();
                            doc.fields
                                .insert(field.name.clone(), serde_json::Value::Array(json_ids));
                        }
                    }
                }
            }
            FieldType::Array => {
                let mut rows =
                    find_array_rows(conn, slug, &field.name, &doc.id, &field.fields, locale_ref)?;
                if rows.is_empty() && fallback_ref.is_some() {
                    rows = find_array_rows(
                        conn,
                        slug,
                        &field.name,
                        &doc.id,
                        &field.fields,
                        fallback_ref,
                    )?;
                }
                doc.fields
                    .insert(field.name.clone(), serde_json::Value::Array(rows));
            }
            FieldType::Group => {
                // Reconstruct nested object from prefixed columns: seo__title → { seo: { title: val } }
                let mut group_obj = serde_json::Map::new();
                let prefix = &field.name;
                reconstruct_group_fields(&field.fields, prefix, doc, &mut group_obj);
                if !group_obj.is_empty() {
                    doc.fields
                        .insert(field.name.clone(), serde_json::Value::Object(group_obj));
                }
            }
            FieldType::Row | FieldType::Collapsible => {
                // Sub-fields are top-level columns, but recurse for join-table types (blocks, arrays, relationships)
                hydrate_document(conn, slug, &field.fields, doc, select, locale_ctx)?;
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    hydrate_document(conn, slug, &tab.fields, doc, select, locale_ctx)?;
                }
            }
            FieldType::Blocks => {
                let mut rows = find_block_rows(conn, slug, &field.name, &doc.id, locale_ref)?;
                if rows.is_empty() && fallback_ref.is_some() {
                    rows = find_block_rows(conn, slug, &field.name, &doc.id, fallback_ref)?;
                }
                doc.fields
                    .insert(field.name.clone(), serde_json::Value::Array(rows));
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
pub(super) mod test_helpers {
    use crate::core::collection::*;
    use crate::core::field::*;
    use rusqlite::Connection;

    pub fn setup_join_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                title TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            -- Has-many junction table
            CREATE TABLE posts_tags (
                parent_id TEXT,
                related_id TEXT,
                _order INTEGER
            );
            -- Array join table
            CREATE TABLE posts_items (
                id TEXT PRIMARY KEY,
                parent_id TEXT,
                _order INTEGER,
                label TEXT,
                value TEXT
            );
            -- Blocks join table
            CREATE TABLE posts_content (
                id TEXT PRIMARY KEY,
                parent_id TEXT,
                _order INTEGER,
                _block_type TEXT,
                data TEXT
            );
            INSERT INTO posts (id, title, created_at, updated_at) VALUES ('p1', 'Post 1', '2024-01-01', '2024-01-01');",
        ).unwrap();
        conn
    }

    pub fn array_sub_fields() -> Vec<FieldDefinition> {
        vec![
            FieldDefinition::builder("label", FieldType::Text).build(),
            FieldDefinition::builder("value", FieldType::Text).build(),
        ]
    }

    pub fn posts_def_with_joins() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("tags", FieldType::Relationship)
                .relationship(RelationshipConfig::new("tags", true))
                .build(),
            FieldDefinition::builder("items", FieldType::Array)
                .fields(array_sub_fields())
                .build(),
            FieldDefinition::builder("content", FieldType::Blocks).build(),
        ];
        def
    }
}

#[cfg(test)]
mod tests {
    use super::super::arrays::{find_array_rows, set_array_rows};
    use super::super::blocks::{find_block_rows, set_block_rows};
    use super::super::relationships::{set_polymorphic_related, set_related_ids};
    use super::test_helpers::{array_sub_fields, posts_def_with_joins, setup_join_db};
    use super::*;
    use crate::core::field::*;
    use rusqlite::Connection;
    use std::collections::HashMap;

    // ── hydrate_document ─────────────────────────────────────────────────────

    #[test]
    fn hydrate_has_many_and_array() {
        let conn = setup_join_db();
        let def = posts_def_with_joins();

        let tag_ids = vec!["t1".to_string(), "t2".to_string()];
        set_related_ids(&conn, "posts", "tags", "p1", &tag_ids, None).unwrap();

        let sub = array_sub_fields();
        let rows = vec![HashMap::from([
            ("label".to_string(), "Item 1".to_string()),
            ("value".to_string(), "Val 1".to_string()),
        ])];
        set_array_rows(&conn, "posts", "items", "p1", &rows, &sub, None).unwrap();

        let blocks = vec![serde_json::json!({"_block_type": "text", "body": "Hello"})];
        set_block_rows(&conn, "posts", "content", "p1", &blocks, None).unwrap();

        let mut doc = crate::core::Document::new("p1".to_string());
        doc.fields
            .insert("title".to_string(), serde_json::json!("Post 1"));

        hydrate_document(&conn, "posts", &def.fields, &mut doc, None, None).unwrap();

        let tags = doc.fields.get("tags").expect("tags should be populated");
        let tags_arr = tags.as_array().expect("tags should be an array");
        assert_eq!(tags_arr.len(), 2);
        assert_eq!(tags_arr[0], "t1");
        assert_eq!(tags_arr[1], "t2");

        let items = doc.fields.get("items").expect("items should be populated");
        let items_arr = items.as_array().expect("items should be an array");
        assert_eq!(items_arr.len(), 1);
        assert_eq!(items_arr[0]["label"], "Item 1");
        assert_eq!(items_arr[0]["value"], "Val 1");

        let content = doc
            .fields
            .get("content")
            .expect("content should be populated");
        let content_arr = content.as_array().expect("content should be an array");
        assert_eq!(content_arr.len(), 1);
        assert_eq!(content_arr[0]["_block_type"], "text");
        assert_eq!(content_arr[0]["body"], "Hello");

        assert_eq!(doc.get_str("title"), Some("Post 1"));
    }

    // ── hydrate_document with select ────────────────────────────────────────

    #[test]
    fn hydrate_with_select_filters_fields() {
        let conn = setup_join_db();
        let def = posts_def_with_joins();

        let tag_ids = vec!["t1".to_string()];
        set_related_ids(&conn, "posts", "tags", "p1", &tag_ids, None).unwrap();
        let sub = array_sub_fields();
        let rows = vec![HashMap::from([
            ("label".to_string(), "Item 1".to_string()),
            ("value".to_string(), "Val 1".to_string()),
        ])];
        set_array_rows(&conn, "posts", "items", "p1", &rows, &sub, None).unwrap();

        let mut doc = crate::core::Document::new("p1".to_string());
        doc.fields
            .insert("title".to_string(), serde_json::json!("Post 1"));

        let select = vec!["tags".to_string(), "title".to_string()];
        hydrate_document(&conn, "posts", &def.fields, &mut doc, Some(&select), None).unwrap();

        assert!(doc.fields.contains_key("tags"), "tags should be hydrated");
        assert!(
            !doc.fields.contains_key("items"),
            "items should NOT be hydrated (not in select)"
        );
        assert!(
            !doc.fields.contains_key("content"),
            "content should NOT be hydrated (not in select)"
        );
    }

    // ── Group field hydration ───────────────────────────────────────────────

    #[test]
    fn hydrate_group_fields() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                title TEXT,
                seo__meta_title TEXT,
                seo__meta_desc TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            INSERT INTO posts VALUES ('p1', 'Test', 'SEO Title', 'SEO Desc', '2024-01-01', '2024-01-01');",
        ).unwrap();

        let fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("meta_title", FieldType::Text).build(),
                    FieldDefinition::builder("meta_desc", FieldType::Text).build(),
                ])
                .build(),
        ];

        let mut doc = Document::new("p1".to_string());
        doc.fields
            .insert("title".to_string(), serde_json::json!("Test"));
        doc.fields.insert(
            "seo__meta_title".to_string(),
            serde_json::json!("SEO Title"),
        );
        doc.fields
            .insert("seo__meta_desc".to_string(), serde_json::json!("SEO Desc"));

        hydrate_document(&conn, "posts", &fields, &mut doc, None, None).unwrap();

        let seo = doc.fields.get("seo").expect("seo group should exist");
        assert_eq!(
            seo.get("meta_title").and_then(|v| v.as_str()),
            Some("SEO Title")
        );
        assert_eq!(
            seo.get("meta_desc").and_then(|v| v.as_str()),
            Some("SEO Desc")
        );
        assert!(!doc.fields.contains_key("seo__meta_title"));
        assert!(!doc.fields.contains_key("seo__meta_desc"));
    }

    #[test]
    fn hydrate_nested_group_fields() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                seo__social__og_title TEXT
            );
            INSERT INTO posts VALUES ('p1', 'OG Title Value');",
        )
        .unwrap();

        let inner_group = FieldDefinition::builder("social", FieldType::Group)
            .fields(vec![
                FieldDefinition::builder("og_title", FieldType::Text).build(),
            ])
            .build();
        let outer_group = FieldDefinition::builder("seo", FieldType::Group)
            .fields(vec![inner_group])
            .build();

        let mut doc = Document::new("p1".to_string());
        doc.fields.insert(
            "seo__social__og_title".to_string(),
            serde_json::json!("OG Title Value"),
        );

        hydrate_document(&conn, "posts", &[outer_group], &mut doc, None, None).unwrap();

        let seo = doc.fields.get("seo").expect("seo group should exist");
        let social = seo.get("social").expect("nested social group should exist");
        assert_eq!(
            social.get("og_title").and_then(|v| v.as_str()),
            Some("OG Title Value")
        );
    }

    #[test]
    fn hydrate_group_with_row_sub_fields() {
        // A Row inside a Group is transparent — its sub-fields are promoted to the group level
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (id TEXT PRIMARY KEY);
             INSERT INTO posts (id) VALUES ('p1');",
        )
        .unwrap();

        let row_wrapper = FieldDefinition::builder("layout_row", FieldType::Row)
            .fields(vec![
                FieldDefinition::builder("col_a", FieldType::Text).build(),
                FieldDefinition::builder("col_b", FieldType::Text).build(),
            ])
            .build();
        let outer_group = FieldDefinition::builder("layout", FieldType::Group)
            .fields(vec![row_wrapper])
            .build();

        let mut doc = Document::new("p1".to_string());
        doc.fields
            .insert("layout__col_a".to_string(), serde_json::json!("A"));
        doc.fields
            .insert("layout__col_b".to_string(), serde_json::json!("B"));

        hydrate_document(&conn, "posts", &[outer_group], &mut doc, None, None).unwrap();

        let layout = doc.fields.get("layout").expect("layout group should exist");
        assert_eq!(layout.get("col_a").and_then(|v| v.as_str()), Some("A"));
        assert_eq!(layout.get("col_b").and_then(|v| v.as_str()), Some("B"));
        assert!(
            layout.get("layout_row").is_none(),
            "Row wrapper should be transparent"
        );
    }

    #[test]
    fn hydrate_group_with_tabs_sub_fields() {
        use crate::core::field::FieldTab;

        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (id TEXT PRIMARY KEY);
             INSERT INTO posts (id) VALUES ('p1');",
        )
        .unwrap();

        let tabs_wrapper = FieldDefinition::builder("tabs", FieldType::Tabs)
            .tabs(vec![
                FieldTab::new(
                    "Tab A",
                    vec![FieldDefinition::builder("field_a", FieldType::Text).build()],
                ),
                FieldTab::new(
                    "Tab B",
                    vec![FieldDefinition::builder("field_b", FieldType::Text).build()],
                ),
            ])
            .build();
        let outer_group = FieldDefinition::builder("settings", FieldType::Group)
            .fields(vec![tabs_wrapper])
            .build();

        let mut doc = Document::new("p1".to_string());
        doc.fields
            .insert("settings__field_a".to_string(), serde_json::json!("val_a"));
        doc.fields
            .insert("settings__field_b".to_string(), serde_json::json!("val_b"));

        hydrate_document(&conn, "posts", &[outer_group], &mut doc, None, None).unwrap();

        let settings = doc
            .fields
            .get("settings")
            .expect("settings group should exist");
        assert_eq!(
            settings.get("field_a").and_then(|v| v.as_str()),
            Some("val_a")
        );
        assert_eq!(
            settings.get("field_b").and_then(|v| v.as_str()),
            Some("val_b")
        );
    }

    // ── Polymorphic hydration ─────────────────────────────────────────────

    #[test]
    fn hydrate_polymorphic_has_many() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (id TEXT PRIMARY KEY);
             CREATE TABLE posts_refs (
                 parent_id TEXT,
                 related_id TEXT,
                 related_collection TEXT NOT NULL DEFAULT '',
                 _order INTEGER,
                 PRIMARY KEY (parent_id, related_id, related_collection)
             );
             INSERT INTO posts (id) VALUES ('p1');",
        )
        .unwrap();

        let items = vec![
            ("articles".to_string(), "a1".to_string()),
            ("pages".to_string(), "pg1".to_string()),
        ];
        set_polymorphic_related(&conn, "posts", "refs", "p1", &items, None).unwrap();

        let mut refs_rel = RelationshipConfig::new("articles", true);
        refs_rel.polymorphic = vec!["articles".to_string(), "pages".to_string()];
        let fields = vec![
            FieldDefinition::builder("refs", FieldType::Relationship)
                .relationship(refs_rel)
                .build(),
        ];

        let mut doc = Document::new("p1".to_string());
        hydrate_document(&conn, "posts", &fields, &mut doc, None, None).unwrap();

        let refs = doc.fields.get("refs").expect("refs should be hydrated");
        let arr = refs.as_array().expect("should be array");
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0].as_str().unwrap(), "articles/a1");
        assert_eq!(arr[1].as_str().unwrap(), "pages/pg1");
    }

    // ── Regression: nested layout field types ────────────────────────────

    #[test]
    fn save_and_hydrate_blocks_inside_tabs() {
        // Regression: blocks nested inside a Tabs field were lost on save and invisible on read
        use super::save::save_join_table_data;
        use crate::core::field::FieldTab;
        let conn = setup_join_db();

        let blocks_field = FieldDefinition::builder("content", FieldType::Blocks).build();
        let tabs_field = FieldDefinition::builder("page_settings", FieldType::Tabs)
            .tabs(vec![FieldTab::new("Content", vec![blocks_field])])
            .build();
        let fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            tabs_field,
        ];

        let mut data = HashMap::new();
        data.insert(
            "content".to_string(),
            serde_json::json!([
                {"_block_type": "hero", "heading": "Welcome"},
                {"_block_type": "text", "body": "Hello world"},
            ]),
        );
        save_join_table_data(&conn, "posts", &fields, "p1", &data, None).unwrap();

        let rows = find_block_rows(&conn, "posts", "content", "p1", None).unwrap();
        assert_eq!(rows.len(), 2, "blocks should be saved through Tabs");
        assert_eq!(rows[0]["_block_type"], "hero");
        assert_eq!(rows[1]["_block_type"], "text");

        let mut doc = Document::new("p1".to_string());
        doc.fields
            .insert("title".to_string(), serde_json::json!("Post 1"));
        hydrate_document(&conn, "posts", &fields, &mut doc, None, None).unwrap();

        let content = doc
            .fields
            .get("content")
            .expect("blocks must be hydrated through Tabs");
        let arr = content.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["_block_type"], "hero");
        assert_eq!(arr[1]["_block_type"], "text");
    }

    #[test]
    fn save_and_hydrate_array_inside_row() {
        // Regression: arrays nested inside a Row field were lost on save and invisible on read
        use super::save::save_join_table_data;
        let conn = setup_join_db();

        let array_field = FieldDefinition::builder("items", FieldType::Array)
            .fields(array_sub_fields())
            .build();
        let row_field = FieldDefinition::builder("main_row", FieldType::Row)
            .fields(vec![array_field])
            .build();
        let fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            row_field,
        ];

        let mut data = HashMap::new();
        data.insert(
            "items".to_string(),
            serde_json::json!([
                {"label": "First", "value": "1"},
                {"label": "Second", "value": "2"},
            ]),
        );
        save_join_table_data(&conn, "posts", &fields, "p1", &data, None).unwrap();

        let rows =
            find_array_rows(&conn, "posts", "items", "p1", &array_sub_fields(), None).unwrap();
        assert_eq!(rows.len(), 2, "array should be saved through Row");
        assert_eq!(rows[0]["label"], "First");
        assert_eq!(rows[1]["label"], "Second");

        let mut doc = Document::new("p1".to_string());
        hydrate_document(&conn, "posts", &fields, &mut doc, None, None).unwrap();

        let items = doc
            .fields
            .get("items")
            .expect("array must be hydrated through Row");
        let arr = items.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["label"], "First");
        assert_eq!(arr[1]["value"], "2");
    }

    #[test]
    fn save_and_hydrate_blocks_inside_collapsible() {
        // Regression: blocks nested inside a Collapsible field were lost
        use super::save::save_join_table_data;
        let conn = setup_join_db();

        let blocks_field = FieldDefinition::builder("content", FieldType::Blocks).build();
        let collapsible_field = FieldDefinition::builder("advanced", FieldType::Collapsible)
            .fields(vec![blocks_field])
            .build();
        let fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            collapsible_field,
        ];

        let mut data = HashMap::new();
        data.insert(
            "content".to_string(),
            serde_json::json!([
                {"_block_type": "cta", "heading": "Buy now"},
            ]),
        );
        save_join_table_data(&conn, "posts", &fields, "p1", &data, None).unwrap();

        let rows = find_block_rows(&conn, "posts", "content", "p1", None).unwrap();
        assert_eq!(rows.len(), 1, "blocks should be saved through Collapsible");
        assert_eq!(rows[0]["_block_type"], "cta");

        let mut doc = Document::new("p1".to_string());
        hydrate_document(&conn, "posts", &fields, &mut doc, None, None).unwrap();

        let content = doc
            .fields
            .get("content")
            .expect("blocks must be hydrated through Collapsible");
        let arr = content.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["_block_type"], "cta");
    }

    // ── Locale fallback tests ─────────────────────────────────────────────

    #[test]
    fn hydrate_fallback_locale_for_has_many() {
        use crate::config::LocaleConfig;

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

        let tags_field = FieldDefinition::builder("tags", FieldType::Relationship)
            .localized(true)
            .relationship(RelationshipConfig::new("tags", true))
            .build();

        let locale_config = LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: true,
        };
        let locale_ctx = LocaleContext {
            mode: LocaleMode::Single("de".to_string()),
            config: locale_config,
        };

        let mut doc = Document::new("p1".to_string());
        hydrate_document(
            &conn,
            "posts",
            &[tags_field],
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
        use crate::config::LocaleConfig;

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

        let locale_config = LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: true,
        };
        let locale_ctx = LocaleContext {
            mode: LocaleMode::Single("de".to_string()),
            config: locale_config,
        };

        let mut doc = Document::new("p1".to_string());
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
        use crate::config::LocaleConfig;

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

        let locale_config = LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: true,
        };
        let locale_ctx = LocaleContext {
            mode: LocaleMode::Single("de".to_string()),
            config: locale_config,
        };

        let mut doc = Document::new("p1".to_string());
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
        use crate::config::LocaleConfig;

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

        let tags_field = FieldDefinition::builder("tags", FieldType::Relationship)
            .localized(true)
            .relationship(RelationshipConfig::new("tags", true))
            .build();

        let locale_config = LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: true,
        };
        let locale_ctx = LocaleContext {
            mode: LocaleMode::Single("de".to_string()),
            config: locale_config,
        };

        let mut doc = Document::new("p1".to_string());
        hydrate_document(
            &conn,
            "posts",
            &[tags_field],
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
        use crate::config::LocaleConfig;

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
        refs_rel.polymorphic = vec!["articles".to_string(), "pages".to_string()];
        let refs_field = FieldDefinition::builder("refs", FieldType::Relationship)
            .localized(true)
            .relationship(refs_rel)
            .build();

        let locale_config = LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: true,
        };
        let locale_ctx = LocaleContext {
            mode: LocaleMode::Single("de".to_string()),
            config: locale_config,
        };

        let mut doc = Document::new("p1".to_string());
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
}
