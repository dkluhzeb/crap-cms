//! Document hydration and join table save orchestration.

mod group;
mod locale;
mod save;

pub use save::save_join_table_data;

use anyhow::Result;
use serde_json::Value;

use super::{
    arrays::find_array_rows,
    blocks::find_block_rows,
    relationships::{find_polymorphic_related, find_related_ids},
};
use crate::{
    core::{Document, FieldDefinition, FieldType},
    db::LocaleContext,
};
use group::reconstruct_group_fields;

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
        if let Some(sel) = select
            && !sel.iter().any(|s| s == &field.name)
        {
            continue;
        }
        let locale = locale::resolve_join_locale(field, locale_ctx);
        let locale_ref = locale.as_deref();
        let fallback_locale = locale::resolve_join_fallback_locale(field, locale_ctx);
        let fallback_ref = fallback_locale.as_deref();
        match field.field_type {
            FieldType::Relationship | FieldType::Upload => {
                if let Some(ref rc) = field.relationship
                    && rc.has_many
                {
                    if rc.is_polymorphic() {
                        let mut items =
                            find_polymorphic_related(conn, slug, &field.name, &doc.id, locale_ref)?;

                        if items.is_empty() && fallback_ref.is_some() {
                            items = find_polymorphic_related(
                                conn,
                                slug,
                                &field.name,
                                &doc.id,
                                fallback_ref,
                            )?;
                        }
                        let json_items: Vec<Value> = items
                            .into_iter()
                            .map(|(col, id)| Value::String(format!("{}/{}", col, id)))
                            .collect();
                        doc.fields
                            .insert(field.name.clone(), Value::Array(json_items));
                    } else {
                        let mut ids =
                            find_related_ids(conn, slug, &field.name, &doc.id, locale_ref)?;

                        if ids.is_empty() && fallback_ref.is_some() {
                            ids = find_related_ids(conn, slug, &field.name, &doc.id, fallback_ref)?;
                        }
                        let json_ids: Vec<Value> = ids.into_iter().map(Value::String).collect();
                        doc.fields
                            .insert(field.name.clone(), Value::Array(json_ids));
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
                doc.fields.insert(field.name.clone(), Value::Array(rows));
            }
            FieldType::Group => {
                // Reconstruct nested object from prefixed columns: seo__title → { seo: { title: val } }
                let mut group_obj = serde_json::Map::new();
                let prefix = &field.name;
                reconstruct_group_fields(&field.fields, prefix, doc, &mut group_obj);

                if !group_obj.is_empty() {
                    doc.fields
                        .insert(field.name.clone(), Value::Object(group_obj));
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
                doc.fields.insert(field.name.clone(), Value::Array(rows));
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
pub(super) mod test_helpers {
    use crate::core::{collection::*, field::*};
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
    use std::collections::HashMap;

    use serde_json::json;

    use super::{
        super::{
            arrays::{find_array_rows, set_array_rows},
            blocks::{find_block_rows, set_block_rows},
            relationships::{set_polymorphic_related, set_related_ids},
        },
        test_helpers::{array_sub_fields, posts_def_with_joins, setup_join_db},
        *,
    };
    use crate::core::{Document, field::*};
    use rusqlite::Connection;

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

        let blocks = vec![json!({"_block_type": "text", "body": "Hello"})];
        set_block_rows(&conn, "posts", "content", "p1", &blocks, None).unwrap();

        let mut doc = Document::new("p1".to_string());
        doc.fields.insert("title".to_string(), json!("Post 1"));

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

        let mut doc = Document::new("p1".to_string());
        doc.fields.insert("title".to_string(), json!("Post 1"));

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
        refs_rel.polymorphic = vec!["articles".into(), "pages".into()];
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
            json!([
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
        doc.fields.insert("title".to_string(), json!("Post 1"));
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
            json!([
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
            json!([
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
}
