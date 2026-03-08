//! Join table data persistence (save operations).

use anyhow::Result;
use std::collections::HashMap;

use crate::core::field::{FieldDefinition, FieldType};
use super::super::super::{LocaleContext};
use super::super::arrays::set_array_rows;
use super::super::blocks::set_block_rows;
use super::super::relationships::{set_polymorphic_related, set_related_ids};
use super::resolve_join_locale;

/// Parse polymorphic relationship values from form data.
/// Accepts "collection/id" composite strings from either a JSON array or comma-separated string.
fn parse_polymorphic_values(val: &serde_json::Value) -> Vec<(String, String)> {
    let raw_items: Vec<String> = match val {
        serde_json::Value::Array(arr) => {
            arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect()
        }
        serde_json::Value::String(s) => {
            if s.is_empty() {
                Vec::new()
            } else {
                s.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
            }
        }
        _ => Vec::new(),
    };
    raw_items.into_iter().filter_map(|item| {
        // Parse "collection/id" format
        if let Some(pos) = item.find('/') {
            let col = item[..pos].to_string();
            let id = item[pos + 1..].to_string();
            if !col.is_empty() && !id.is_empty() {
                return Some((col, id));
            }
        }
        None
    }).collect()
}

/// Save join table data for has-many relationships and arrays.
/// Extracts relevant data from the data map and writes to join tables.
/// When `locale_ctx` is provided, localized join fields are scoped by locale.
pub fn save_join_table_data(
    conn: &rusqlite::Connection,
    slug: &str,
    fields: &[FieldDefinition],
    parent_id: &str,
    data: &HashMap<String, serde_json::Value>,
    locale_ctx: Option<&LocaleContext>,
) -> Result<()> {
    for field in fields {
        let locale = resolve_join_locale(field, locale_ctx);
        let locale_ref = locale.as_deref();
        match field.field_type {
            FieldType::Relationship | FieldType::Upload => {
                if let Some(ref rc) = field.relationship {
                    if rc.has_many {
                        // Only touch join table if the field was explicitly included in the data.
                        if let Some(val) = data.get(&field.name) {
                            if rc.is_polymorphic() {
                                // Polymorphic: values are "collection/id" composite strings
                                let items = parse_polymorphic_values(val);
                                set_polymorphic_related(conn, slug, &field.name, parent_id, &items, locale_ref)?;
                            } else {
                                let ids = match val {
                                    serde_json::Value::Array(arr) => {
                                        arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect()
                                    }
                                    serde_json::Value::String(s) => {
                                        if s.is_empty() {
                                            Vec::new()
                                        } else {
                                            s.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
                                        }
                                    }
                                    _ => Vec::new(),
                                };
                                set_related_ids(conn, slug, &field.name, parent_id, &ids, locale_ref)?;
                            }
                        }
                    }
                }
            }
            FieldType::Array => {
                if let Some(val) = data.get(&field.name) {
                    let rows = match val {
                        serde_json::Value::Array(arr) => {
                            arr.iter().filter_map(|v| {
                                if let serde_json::Value::Object(map) = v {
                                    let row: HashMap<String, String> = map.iter().map(|(k, v)| {
                                        let s = match v {
                                            serde_json::Value::String(s) => s.clone(),
                                            other => other.to_string(),
                                        };
                                        (k.clone(), s)
                                    }).collect();
                                    Some(row)
                                } else {
                                    None
                                }
                            }).collect()
                        }
                        _ => Vec::new(),
                    };
                    set_array_rows(conn, slug, &field.name, parent_id, &rows, &field.fields, locale_ref)?;
                }
            }
            FieldType::Blocks => {
                if let Some(val) = data.get(&field.name) {
                    let rows = match val {
                        serde_json::Value::Array(arr) => arr.clone(),
                        _ => Vec::new(),
                    };
                    set_block_rows(conn, slug, &field.name, parent_id, &rows, locale_ref)?;
                }
            }
            FieldType::Row | FieldType::Collapsible => {
                save_join_table_data(conn, slug, &field.fields, parent_id, data, locale_ctx)?;
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    save_join_table_data(conn, slug, &tab.fields, parent_id, data, locale_ctx)?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use crate::core::field::*;
    use super::super::super::relationships::{find_related_ids, find_polymorphic_related, set_related_ids};
    use super::super::super::blocks::find_block_rows;
    use super::super::test_helpers::{setup_join_db, posts_def_with_joins};

    #[test]
    fn save_join_table_data_has_many_from_string() {
        let conn = setup_join_db();
        let def = posts_def_with_joins();

        let mut data = HashMap::new();
        data.insert("tags".to_string(), serde_json::json!("t1, t2, t3"));

        save_join_table_data(&conn, "posts", &def.fields, "p1", &data, None).unwrap();

        let found = find_related_ids(&conn, "posts", "tags", "p1", None).unwrap();
        assert_eq!(found, vec!["t1", "t2", "t3"]);
    }

    #[test]
    fn save_join_table_data_has_many_from_array() {
        let conn = setup_join_db();
        let def = posts_def_with_joins();

        let mut data = HashMap::new();
        data.insert("tags".to_string(), serde_json::json!(["t1", "t2"]));

        save_join_table_data(&conn, "posts", &def.fields, "p1", &data, None).unwrap();

        let found = find_related_ids(&conn, "posts", "tags", "p1", None).unwrap();
        assert_eq!(found, vec!["t1", "t2"]);
    }

    #[test]
    fn save_join_table_data_has_many_empty_string() {
        let conn = setup_join_db();
        let def = posts_def_with_joins();

        set_related_ids(&conn, "posts", "tags", "p1", &["t1".to_string()], None).unwrap();

        let mut data = HashMap::new();
        data.insert("tags".to_string(), serde_json::json!(""));

        save_join_table_data(&conn, "posts", &def.fields, "p1", &data, None).unwrap();

        let found = find_related_ids(&conn, "posts", "tags", "p1", None).unwrap();
        assert!(found.is_empty());
    }

    #[test]
    fn save_join_table_data_blocks() {
        let conn = setup_join_db();
        let def = posts_def_with_joins();

        let mut data = HashMap::new();
        data.insert("content".to_string(), serde_json::json!([
            {"_block_type": "paragraph", "text": "Hello"},
            {"_block_type": "image", "url": "/img.jpg"},
        ]));

        save_join_table_data(&conn, "posts", &def.fields, "p1", &data, None).unwrap();

        let found = find_block_rows(&conn, "posts", "content", "p1", None).unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(found[0]["_block_type"], "paragraph");
        assert_eq!(found[1]["_block_type"], "image");
    }

    #[test]
    fn save_join_table_data_skips_absent_fields() {
        let conn = setup_join_db();
        let def = posts_def_with_joins();

        set_related_ids(&conn, "posts", "tags", "p1", &["t1".to_string()], None).unwrap();

        let data = HashMap::new();

        save_join_table_data(&conn, "posts", &def.fields, "p1", &data, None).unwrap();

        let found = find_related_ids(&conn, "posts", "tags", "p1", None).unwrap();
        assert_eq!(found, vec!["t1"], "tags should be preserved when not in data");
    }

    #[test]
    fn save_join_table_data_blocks_non_array_is_noop() {
        let conn = setup_join_db();
        let def = posts_def_with_joins();

        let mut data = HashMap::new();
        data.insert("content".to_string(), serde_json::json!("not an array"));
        save_join_table_data(&conn, "posts", &def.fields, "p1", &data, None).unwrap();

        let found = find_block_rows(&conn, "posts", "content", "p1", None).unwrap();
        assert!(found.is_empty());
    }

    #[test]
    fn save_join_table_data_has_many_non_string_non_array_is_empty() {
        let conn = setup_join_db();
        let def = posts_def_with_joins();

        let mut data = HashMap::new();
        data.insert("tags".to_string(), serde_json::json!(42));
        save_join_table_data(&conn, "posts", &def.fields, "p1", &data, None).unwrap();

        let found = find_related_ids(&conn, "posts", "tags", "p1", None).unwrap();
        assert!(found.is_empty());
    }

    #[test]
    fn save_join_table_data_polymorphic_has_many() {
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
        ).unwrap();

        let mut refs_rel = RelationshipConfig::new("articles", true);
        refs_rel.polymorphic = vec!["articles".to_string(), "pages".to_string()];
        let fields = vec![
            FieldDefinition {
                name: "refs".to_string(),
                field_type: FieldType::Relationship,
                relationship: Some(refs_rel),
                ..Default::default()
            },
        ];

        let mut data = HashMap::new();
        data.insert("refs".to_string(), serde_json::json!("articles/a1,pages/pg1"));

        save_join_table_data(&conn, "posts", &fields, "p1", &data, None).unwrap();

        let found = find_polymorphic_related(&conn, "posts", "refs", "p1", None).unwrap();
        assert_eq!(found, vec![
            ("articles".to_string(), "a1".to_string()),
            ("pages".to_string(), "pg1".to_string()),
        ]);
    }

    #[test]
    fn parse_polymorphic_values_from_json_array() {
        let val = serde_json::json!(["articles/a1", "pages/pg1"]);
        let items = parse_polymorphic_values(&val);
        assert_eq!(items, vec![
            ("articles".to_string(), "a1".to_string()),
            ("pages".to_string(), "pg1".to_string()),
        ]);
    }

    #[test]
    fn parse_polymorphic_values_from_comma_string() {
        let val = serde_json::json!("articles/a1,pages/pg1");
        let items = parse_polymorphic_values(&val);
        assert_eq!(items, vec![
            ("articles".to_string(), "a1".to_string()),
            ("pages".to_string(), "pg1".to_string()),
        ]);
    }

    #[test]
    fn parse_polymorphic_values_skips_invalid() {
        let val = serde_json::json!(["articles/a1", "no_slash", "", "pages/"]);
        let items = parse_polymorphic_values(&val);
        assert_eq!(items, vec![
            ("articles".to_string(), "a1".to_string()),
        ], "Should skip entries without valid collection/id format");
    }

    #[test]
    fn parse_polymorphic_values_from_null() {
        let val = serde_json::Value::Null;
        let items = parse_polymorphic_values(&val);
        assert!(items.is_empty(), "null input should yield no items");
    }

    #[test]
    fn parse_polymorphic_values_from_number() {
        let val = serde_json::json!(42);
        let items = parse_polymorphic_values(&val);
        assert!(items.is_empty(), "number input should yield no items");
    }

    #[test]
    fn parse_polymorphic_values_empty_string() {
        let val = serde_json::json!("");
        let items = parse_polymorphic_values(&val);
        assert!(items.is_empty(), "empty string should yield no items");
    }

    #[test]
    fn parse_polymorphic_values_slash_prefix_only() {
        let val = serde_json::json!(["articles/"]);
        let items = parse_polymorphic_values(&val);
        assert!(items.is_empty(), "/id empty should be skipped");
    }
}
