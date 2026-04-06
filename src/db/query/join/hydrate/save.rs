//! Join table data persistence (save operations).

use anyhow::Result;
use serde_json::Value;
use std::collections::HashMap;

use super::{
    super::{
        arrays::set_array_rows,
        blocks::set_block_rows,
        relationships::{set_polymorphic_related, set_related_ids},
    },
    locale::resolve_join_locale,
};
use crate::{
    core::{FieldDefinition, FieldType},
    db::{DbConnection, LocaleContext, query::helpers::prefixed_name},
};

/// Parse a JSON value into a list of string IDs.
/// Accepts a JSON array of strings or a comma-separated string.
fn parse_id_list(val: &Value) -> Vec<String> {
    match val {
        Value::Array(arr) => arr
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect(),
        Value::String(s) => {
            if s.is_empty() {
                Vec::new()
            } else {
                s.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            }
        }
        _ => Vec::new(),
    }
}

/// Parse polymorphic relationship values from form data.
/// Accepts "collection/id" composite strings from either a JSON array or comma-separated string.
fn parse_polymorphic_values(val: &Value) -> Vec<(String, String)> {
    parse_id_list(val)
        .into_iter()
        .filter_map(|item| {
            let (col, id) = item.split_once('/')?;

            if !col.is_empty() && !id.is_empty() {
                Some((col.to_string(), id.to_string()))
            } else {
                None
            }
        })
        .collect()
}

/// Coerce a JSON array of objects into a list of string-keyed HashMaps.
fn coerce_array_rows(val: &Value) -> Vec<HashMap<String, String>> {
    match val {
        Value::Array(arr) => arr
            .iter()
            .filter_map(|v| {
                let map = v.as_object()?;
                let row = map
                    .iter()
                    .map(|(k, v)| {
                        let s = match v {
                            Value::String(s) => s.clone(),
                            other => other.to_string(),
                        };
                        (k.clone(), s)
                    })
                    .collect();
                Some(row)
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Save join table data for has-many relationships and arrays.
/// Extracts relevant data from the data map and writes to join tables.
/// When `locale_ctx` is provided, localized join fields are scoped by locale.
pub fn save_join_table_data(
    conn: &dyn DbConnection,
    slug: &str,
    fields: &[FieldDefinition],
    parent_id: &str,
    data: &HashMap<String, Value>,
    locale_ctx: Option<&LocaleContext>,
) -> Result<()> {
    save_join_data_inner(conn, slug, fields, parent_id, data, locale_ctx, "")
}

fn save_join_data_inner(
    conn: &dyn DbConnection,
    slug: &str,
    fields: &[FieldDefinition],
    parent_id: &str,
    data: &HashMap<String, Value>,
    locale_ctx: Option<&LocaleContext>,
    prefix: &str,
) -> Result<()> {
    for field in fields {
        let locale = resolve_join_locale(field, locale_ctx);
        let locale_ref = locale.as_deref();
        let field_key = prefixed_name(prefix, &field.name);
        match field.field_type {
            FieldType::Relationship | FieldType::Upload => {
                if let Some(ref rc) = field.relationship
                    && rc.has_many
                    && let Some(val) = data.get(&field_key)
                {
                    if rc.is_polymorphic() {
                        let items = parse_polymorphic_values(val);
                        set_polymorphic_related(
                            conn, slug, &field_key, parent_id, &items, locale_ref,
                        )?;
                    } else {
                        let ids = parse_id_list(val);
                        set_related_ids(conn, slug, &field_key, parent_id, &ids, locale_ref)?;
                    }
                }
            }
            FieldType::Array => {
                if let Some(val) = data.get(&field_key) {
                    let rows = coerce_array_rows(val);
                    set_array_rows(
                        conn,
                        slug,
                        &field_key,
                        parent_id,
                        &rows,
                        &field.fields,
                        locale_ref,
                    )?;
                }
            }
            FieldType::Blocks => {
                if let Some(val) = data.get(&field_key) {
                    let rows = match val {
                        Value::Array(arr) => arr.clone(),
                        _ => Vec::new(),
                    };
                    set_block_rows(conn, slug, &field_key, parent_id, &rows, locale_ref)?;
                }
            }
            FieldType::Group => {
                save_join_data_inner(
                    conn,
                    slug,
                    &field.fields,
                    parent_id,
                    data,
                    locale_ctx,
                    &field_key,
                )?;
            }
            FieldType::Row | FieldType::Collapsible => {
                save_join_data_inner(
                    conn,
                    slug,
                    &field.fields,
                    parent_id,
                    data,
                    locale_ctx,
                    prefix,
                )?;
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    save_join_data_inner(
                        conn,
                        slug,
                        &tab.fields,
                        parent_id,
                        data,
                        locale_ctx,
                        prefix,
                    )?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::super::blocks::find_block_rows;
    use super::super::super::relationships::{
        find_polymorphic_related, find_related_ids, set_related_ids,
    };
    use super::super::test_helpers::{posts_def_with_joins, setup_join_db};
    use super::*;
    use crate::config::CrapConfig;
    use crate::core::field::*;
    use crate::db::{BoxedConnection, pool};
    use tempfile::TempDir;

    fn setup_conn(sql: &str) -> (TempDir, BoxedConnection) {
        let dir = TempDir::new().unwrap();
        let config = CrapConfig::default();
        let p = pool::create_pool(dir.path(), &config).unwrap();
        let conn = p.get().unwrap();
        conn.execute_batch(sql).unwrap();
        (dir, conn)
    }

    #[test]
    fn save_join_table_data_has_many_from_string() {
        let (_dir, conn) = setup_join_db();
        let def = posts_def_with_joins();

        let mut data = HashMap::new();
        data.insert("tags".to_string(), json!("t1, t2, t3"));

        save_join_table_data(&conn, "posts", &def.fields, "p1", &data, None).unwrap();

        let found = find_related_ids(&conn, "posts", "tags", "p1", None).unwrap();
        assert_eq!(found, vec!["t1", "t2", "t3"]);
    }

    #[test]
    fn save_join_table_data_has_many_from_array() {
        let (_dir, conn) = setup_join_db();
        let def = posts_def_with_joins();

        let mut data = HashMap::new();
        data.insert("tags".to_string(), json!(["t1", "t2"]));

        save_join_table_data(&conn, "posts", &def.fields, "p1", &data, None).unwrap();

        let found = find_related_ids(&conn, "posts", "tags", "p1", None).unwrap();
        assert_eq!(found, vec!["t1", "t2"]);
    }

    #[test]
    fn save_join_table_data_has_many_empty_string() {
        let (_dir, conn) = setup_join_db();
        let def = posts_def_with_joins();

        set_related_ids(&conn, "posts", "tags", "p1", &["t1".to_string()], None).unwrap();

        let mut data = HashMap::new();
        data.insert("tags".to_string(), json!(""));

        save_join_table_data(&conn, "posts", &def.fields, "p1", &data, None).unwrap();

        let found = find_related_ids(&conn, "posts", "tags", "p1", None).unwrap();
        assert!(found.is_empty());
    }

    #[test]
    fn save_join_table_data_blocks() {
        let (_dir, conn) = setup_join_db();
        let def = posts_def_with_joins();

        let mut data = HashMap::new();
        data.insert(
            "content".to_string(),
            json!([
                {"_block_type": "paragraph", "text": "Hello"},
                {"_block_type": "image", "url": "/img.jpg"},
            ]),
        );

        save_join_table_data(&conn, "posts", &def.fields, "p1", &data, None).unwrap();

        let found = find_block_rows(&conn, "posts", "content", "p1", None).unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(found[0]["_block_type"], "paragraph");
        assert_eq!(found[1]["_block_type"], "image");
    }

    #[test]
    fn save_join_table_data_skips_absent_fields() {
        let (_dir, conn) = setup_join_db();
        let def = posts_def_with_joins();

        set_related_ids(&conn, "posts", "tags", "p1", &["t1".to_string()], None).unwrap();

        let data = HashMap::new();

        save_join_table_data(&conn, "posts", &def.fields, "p1", &data, None).unwrap();

        let found = find_related_ids(&conn, "posts", "tags", "p1", None).unwrap();
        assert_eq!(
            found,
            vec!["t1"],
            "tags should be preserved when not in data"
        );
    }

    #[test]
    fn save_join_table_data_blocks_non_array_is_noop() {
        let (_dir, conn) = setup_join_db();
        let def = posts_def_with_joins();

        let mut data = HashMap::new();
        data.insert("content".to_string(), json!("not an array"));
        save_join_table_data(&conn, "posts", &def.fields, "p1", &data, None).unwrap();

        let found = find_block_rows(&conn, "posts", "content", "p1", None).unwrap();
        assert!(found.is_empty());
    }

    #[test]
    fn save_join_table_data_has_many_non_string_non_array_is_empty() {
        let (_dir, conn) = setup_join_db();
        let def = posts_def_with_joins();

        let mut data = HashMap::new();
        data.insert("tags".to_string(), json!(42));
        save_join_table_data(&conn, "posts", &def.fields, "p1", &data, None).unwrap();

        let found = find_related_ids(&conn, "posts", "tags", "p1", None).unwrap();
        assert!(found.is_empty());
    }

    #[test]
    fn save_join_table_data_polymorphic_has_many() {
        let (_dir, conn) = setup_conn(
            "CREATE TABLE posts (id TEXT PRIMARY KEY);
             CREATE TABLE posts_refs (
                 parent_id TEXT,
                 related_id TEXT,
                 related_collection TEXT NOT NULL DEFAULT '',
                 _order INTEGER,
                 PRIMARY KEY (parent_id, related_id, related_collection)
             );
             INSERT INTO posts (id) VALUES ('p1');",
        );

        let mut refs_rel = RelationshipConfig::new("articles", true);
        refs_rel.polymorphic = vec!["articles".into(), "pages".into()];
        let fields = vec![
            FieldDefinition::builder("refs", FieldType::Relationship)
                .relationship(refs_rel)
                .build(),
        ];

        let mut data = HashMap::new();
        data.insert("refs".to_string(), json!("articles/a1,pages/pg1"));

        save_join_table_data(&conn, "posts", &fields, "p1", &data, None).unwrap();

        let found = find_polymorphic_related(&conn, "posts", "refs", "p1", None).unwrap();
        assert_eq!(
            found,
            vec![
                ("articles".to_string(), "a1".to_string()),
                ("pages".to_string(), "pg1".to_string()),
            ]
        );
    }

    // ── Group > Array/Blocks save ──────────────────────────────────────

    #[test]
    fn save_group_array_data() {
        use super::super::super::arrays::find_array_rows;

        let (_dir, conn) = setup_conn(
            "CREATE TABLE posts (id TEXT PRIMARY KEY);
             CREATE TABLE posts_config__items (
                 id TEXT PRIMARY KEY,
                 parent_id TEXT,
                 _order INTEGER,
                 label TEXT,
                 value TEXT
             );
             INSERT INTO posts (id) VALUES ('p1');",
        );

        let fields = vec![
            FieldDefinition::builder("config", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("items", FieldType::Array)
                        .fields(vec![
                            FieldDefinition::builder("label", FieldType::Text).build(),
                            FieldDefinition::builder("value", FieldType::Text).build(),
                        ])
                        .build(),
                ])
                .build(),
        ];

        let mut data = HashMap::new();
        data.insert(
            "config__items".to_string(),
            json!([{"label": "A", "value": "1"}]),
        );

        save_join_table_data(&conn, "posts", &fields, "p1", &data, None).unwrap();

        let sub = vec![
            FieldDefinition::builder("label", FieldType::Text).build(),
            FieldDefinition::builder("value", FieldType::Text).build(),
        ];
        let rows = find_array_rows(&conn, "posts", "config__items", "p1", &sub, None).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["label"], "A");
        assert_eq!(rows[0]["value"], "1");
    }

    #[test]
    fn save_group_blocks_data() {
        let (_dir, conn) = setup_conn(
            "CREATE TABLE posts (id TEXT PRIMARY KEY);
             CREATE TABLE posts_config__content (
                 id TEXT PRIMARY KEY,
                 parent_id TEXT,
                 _order INTEGER,
                 _block_type TEXT,
                 data TEXT
             );
             INSERT INTO posts (id) VALUES ('p1');",
        );

        let fields = vec![
            FieldDefinition::builder("config", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("content", FieldType::Blocks).build(),
                ])
                .build(),
        ];

        let mut data = HashMap::new();
        data.insert(
            "config__content".to_string(),
            json!([{"_block_type": "hero", "heading": "Hi"}]),
        );

        save_join_table_data(&conn, "posts", &fields, "p1", &data, None).unwrap();

        let rows = find_block_rows(&conn, "posts", "config__content", "p1", None).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["_block_type"], "hero");
        assert_eq!(rows[0]["heading"], "Hi");
    }

    #[test]
    fn save_group_relationship_data() {
        use super::super::super::relationships::find_related_ids;

        let (_dir, conn) = setup_conn(
            "CREATE TABLE posts (id TEXT PRIMARY KEY);
             CREATE TABLE posts_config__tags (
                 parent_id TEXT,
                 related_id TEXT,
                 _order INTEGER,
                 PRIMARY KEY (parent_id, related_id)
             );
             INSERT INTO posts (id) VALUES ('p1');",
        );

        let fields = vec![
            FieldDefinition::builder("config", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("tags", FieldType::Relationship)
                        .relationship(RelationshipConfig::new("tags", true))
                        .build(),
                ])
                .build(),
        ];

        let mut data = HashMap::new();
        data.insert("config__tags".to_string(), json!(["t1", "t2"]));

        save_join_table_data(&conn, "posts", &fields, "p1", &data, None).unwrap();

        let found = find_related_ids(&conn, "posts", "config__tags", "p1", None).unwrap();
        assert_eq!(found, vec!["t1", "t2"]);
    }

    #[test]
    fn save_group_group_array_data() {
        use super::super::super::arrays::find_array_rows;

        let (_dir, conn) = setup_conn(
            "CREATE TABLE posts (id TEXT PRIMARY KEY);
             CREATE TABLE posts_outer__inner__items (
                 id TEXT PRIMARY KEY,
                 parent_id TEXT,
                 _order INTEGER,
                 name TEXT
             );
             INSERT INTO posts (id) VALUES ('p1');",
        );

        let fields = vec![
            FieldDefinition::builder("outer", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("inner", FieldType::Group)
                        .fields(vec![
                            FieldDefinition::builder("items", FieldType::Array)
                                .fields(vec![
                                    FieldDefinition::builder("name", FieldType::Text).build(),
                                ])
                                .build(),
                        ])
                        .build(),
                ])
                .build(),
        ];

        let mut data = HashMap::new();
        data.insert(
            "outer__inner__items".to_string(),
            json!([{"name": "DeepItem"}]),
        );

        save_join_table_data(&conn, "posts", &fields, "p1", &data, None).unwrap();

        let sub = vec![FieldDefinition::builder("name", FieldType::Text).build()];
        let rows =
            find_array_rows(&conn, "posts", "outer__inner__items", "p1", &sub, None).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["name"], "DeepItem");
    }

    #[test]
    fn parse_polymorphic_values_from_json_array() {
        let val = json!(["articles/a1", "pages/pg1"]);
        let items = parse_polymorphic_values(&val);
        assert_eq!(
            items,
            vec![
                ("articles".to_string(), "a1".to_string()),
                ("pages".to_string(), "pg1".to_string()),
            ]
        );
    }

    #[test]
    fn parse_polymorphic_values_from_comma_string() {
        let val = json!("articles/a1,pages/pg1");
        let items = parse_polymorphic_values(&val);
        assert_eq!(
            items,
            vec![
                ("articles".to_string(), "a1".to_string()),
                ("pages".to_string(), "pg1".to_string()),
            ]
        );
    }

    #[test]
    fn parse_polymorphic_values_skips_invalid() {
        let val = json!(["articles/a1", "no_slash", "", "pages/"]);
        let items = parse_polymorphic_values(&val);
        assert_eq!(
            items,
            vec![("articles".to_string(), "a1".to_string()),],
            "Should skip entries without valid collection/id format"
        );
    }

    #[test]
    fn parse_polymorphic_values_from_null() {
        let val = Value::Null;
        let items = parse_polymorphic_values(&val);
        assert!(items.is_empty(), "null input should yield no items");
    }

    #[test]
    fn parse_polymorphic_values_from_number() {
        let val = json!(42);
        let items = parse_polymorphic_values(&val);
        assert!(items.is_empty(), "number input should yield no items");
    }

    #[test]
    fn parse_polymorphic_values_empty_string() {
        let val = json!("");
        let items = parse_polymorphic_values(&val);
        assert!(items.is_empty(), "empty string should yield no items");
    }

    #[test]
    fn parse_polymorphic_values_slash_prefix_only() {
        let val = json!(["articles/"]);
        let items = parse_polymorphic_values(&val);
        assert!(items.is_empty(), "/id empty should be skipped");
    }
}
