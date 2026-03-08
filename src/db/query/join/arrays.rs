//! Array field join table operations.

use anyhow::Result;
use std::collections::HashMap;

use super::super::coerce_value;
use crate::core::field::{FieldDefinition, FieldType};

/// Set array rows for an array field join table.
/// Deletes all existing rows for the parent and inserts new ones with nanoid + _order.
/// When `locale` is Some, scopes the DELETE to that locale and includes `_locale` in INSERT.
pub fn set_array_rows(
    conn: &rusqlite::Connection,
    collection: &str,
    field_name: &str,
    parent_id: &str,
    rows: &[HashMap<String, String>],
    sub_fields: &[FieldDefinition],
    locale: Option<&str>,
) -> Result<()> {
    let table_name = format!("{}_{}", collection, field_name);
    if let Some(loc) = locale {
        conn.execute(
            &format!("DELETE FROM {} WHERE parent_id = ?1 AND _locale = ?2", table_name),
            rusqlite::params![parent_id, loc],
        )?;
    } else {
        conn.execute(
            &format!("DELETE FROM {} WHERE parent_id = ?1", table_name),
            [parent_id],
        )?;
    }

    let flat_subs = crate::core::field::flatten_array_sub_fields(sub_fields);

    if rows.is_empty() || flat_subs.is_empty() {
        return Ok(());
    }

    // Build column list from flattened sub-fields
    let col_names: Vec<&str> = flat_subs.iter().map(|f| f.name.as_str()).collect();
    let (all_cols, placeholders) = if locale.is_some() {
        let all_cols = format!(
            "id, parent_id, _order, _locale, {}",
            col_names.join(", ")
        );
        let placeholders = format!(
            "?1, ?2, ?3, ?4, {}",
            (5..5 + col_names.len()).map(|i| format!("?{}", i)).collect::<Vec<_>>().join(", ")
        );
        (all_cols, placeholders)
    } else {
        let all_cols = format!(
            "id, parent_id, _order, {}",
            col_names.join(", ")
        );
        let placeholders = format!(
            "?1, ?2, ?3, {}",
            (4..4 + col_names.len()).map(|i| format!("?{}", i)).collect::<Vec<_>>().join(", ")
        );
        (all_cols, placeholders)
    };
    let sql = format!("INSERT INTO {} ({}) VALUES ({})", table_name, all_cols, placeholders);

    let mut stmt = conn.prepare(&sql)?;
    for (order, row) in rows.iter().enumerate() {
        let id = nanoid::nanoid!();
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = vec![
            Box::new(id),
            Box::new(parent_id.to_string()),
            Box::new(order as i64),
        ];
        if let Some(loc) = locale {
            params.push(Box::new(loc.to_string()));
        }
        for sf in &flat_subs {
            let value = row.get(&sf.name).cloned().unwrap_or_default();
            params.push(coerce_value(&sf.field_type, &value));
        }
        let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
        stmt.execute(rusqlite::params_from_iter(param_refs.iter()))?;
    }
    Ok(())
}

/// Find array rows for an array field join table, ordered.
/// When `locale` is Some, filters by `_locale`.
pub fn find_array_rows(
    conn: &rusqlite::Connection,
    collection: &str,
    field_name: &str,
    parent_id: &str,
    sub_fields: &[FieldDefinition],
    locale: Option<&str>,
) -> Result<Vec<serde_json::Value>> {
    let table_name = format!("{}_{}", collection, field_name);
    let flat_subs = crate::core::field::flatten_array_sub_fields(sub_fields);
    let col_names: Vec<&str> = flat_subs.iter().map(|f| f.name.as_str()).collect();
    let select_cols = if col_names.is_empty() {
        "id".to_string()
    } else {
        format!("id, {}", col_names.join(", "))
    };
    let sql = if locale.is_some() {
        format!(
            "SELECT {} FROM {} WHERE parent_id = ?1 AND _locale = ?2 ORDER BY _order",
            select_cols, table_name
        )
    } else {
        format!(
            "SELECT {} FROM {} WHERE parent_id = ?1 ORDER BY _order",
            select_cols, table_name
        )
    };
    let mut stmt = conn.prepare(&sql)?;
    let params: Vec<Box<dyn rusqlite::types::ToSql>> = if let Some(loc) = locale {
        vec![Box::new(parent_id.to_string()), Box::new(loc.to_string())]
    } else {
        vec![Box::new(parent_id.to_string())]
    };
    let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let rows = stmt.query_map(rusqlite::params_from_iter(param_refs.iter()), |row| {
        let mut map = serde_json::Map::new();
        let id: String = row.get(0)?;
        map.insert("id".to_string(), serde_json::Value::String(id));
        for (i, sf) in flat_subs.iter().enumerate() {
            let val: rusqlite::types::Value = row.get(i + 1)?;
            let json_val = match val {
                rusqlite::types::Value::Null => serde_json::Value::Null,
                rusqlite::types::Value::Integer(n) => serde_json::json!(n),
                rusqlite::types::Value::Real(f) => serde_json::json!(f),
                rusqlite::types::Value::Text(s) => {
                    // Composite sub-fields store JSON in TEXT columns —
                    // attempt to parse so nested data comes back structured.
                    match sf.field_type {
                        FieldType::Array | FieldType::Blocks | FieldType::Group | FieldType::Row | FieldType::Collapsible | FieldType::Tabs | FieldType::Json => {
                            serde_json::from_str(&s).unwrap_or(serde_json::Value::String(s))
                        }
                        _ => serde_json::Value::String(s),
                    }
                }
                rusqlite::types::Value::Blob(_) => serde_json::Value::Null,
            };
            map.insert(sf.name.clone(), json_val);
        }
        Ok(serde_json::Value::Object(map))
    })?.filter_map(|r| r.ok()).collect();
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use crate::core::field::FieldTab;

    fn setup_array_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (id TEXT PRIMARY KEY);
             CREATE TABLE posts_items (
                 id TEXT PRIMARY KEY,
                 parent_id TEXT,
                 _order INTEGER,
                 label TEXT,
                 value TEXT
             );
             INSERT INTO posts (id) VALUES ('p1');",
        ).unwrap();
        conn
    }

    fn array_sub_fields() -> Vec<FieldDefinition> {
        vec![
            FieldDefinition {
                name: "label".to_string(),
                ..Default::default()
            },
            FieldDefinition {
                name: "value".to_string(),
                ..Default::default()
            },
        ]
    }

    // ── set_array_rows + find_array_rows ─────────────────────────────────────

    #[test]
    fn set_and_find_array_rows() {
        let conn = setup_array_db();
        let sub = array_sub_fields();
        let rows = vec![
            HashMap::from([
                ("label".to_string(), "Label A".to_string()),
                ("value".to_string(), "Value A".to_string()),
            ]),
            HashMap::from([
                ("label".to_string(), "Label B".to_string()),
                ("value".to_string(), "Value B".to_string()),
            ]),
        ];
        set_array_rows(&conn, "posts", "items", "p1", &rows, &sub, None).unwrap();

        let found = find_array_rows(&conn, "posts", "items", "p1", &sub, None).unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(found[0]["label"], "Label A");
        assert_eq!(found[0]["value"], "Value A");
        assert_eq!(found[1]["label"], "Label B");
        assert_eq!(found[1]["value"], "Value B");
        assert!(found[0]["id"].as_str().is_some(), "Row should have an id");
        assert!(found[1]["id"].as_str().is_some(), "Row should have an id");
    }

    #[test]
    fn replace_array_rows() {
        let conn = setup_array_db();
        let sub = array_sub_fields();
        let rows_old = vec![
            HashMap::from([
                ("label".to_string(), "Old".to_string()),
                ("value".to_string(), "Old Val".to_string()),
            ]),
        ];
        set_array_rows(&conn, "posts", "items", "p1", &rows_old, &sub, None).unwrap();

        let rows_new = vec![
            HashMap::from([
                ("label".to_string(), "New".to_string()),
                ("value".to_string(), "New Val".to_string()),
            ]),
        ];
        set_array_rows(&conn, "posts", "items", "p1", &rows_new, &sub, None).unwrap();

        let found = find_array_rows(&conn, "posts", "items", "p1", &sub, None).unwrap();
        assert_eq!(found.len(), 1, "Old rows should be replaced");
        assert_eq!(found[0]["label"], "New");
        assert_eq!(found[0]["value"], "New Val");
    }

    #[test]
    fn empty_array_rows() {
        let conn = setup_array_db();
        let sub = array_sub_fields();
        let rows = vec![
            HashMap::from([
                ("label".to_string(), "X".to_string()),
                ("value".to_string(), "Y".to_string()),
            ]),
        ];
        set_array_rows(&conn, "posts", "items", "p1", &rows, &sub, None).unwrap();
        set_array_rows(&conn, "posts", "items", "p1", &[], &sub, None).unwrap();

        let found = find_array_rows(&conn, "posts", "items", "p1", &sub, None).unwrap();
        assert!(found.is_empty(), "Should return empty after setting empty rows");
    }

    #[test]
    fn set_and_find_array_rows_with_tabs() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (id TEXT PRIMARY KEY);
             CREATE TABLE posts_items (
                 id TEXT PRIMARY KEY,
                 parent_id TEXT,
                 _order INTEGER,
                 title TEXT,
                 body TEXT
             );
             INSERT INTO posts (id) VALUES ('p1');",
        ).unwrap();

        // Sub-fields wrapped in Tabs
        let sub_fields = vec![
            FieldDefinition {
                name: "layout".to_string(),
                field_type: FieldType::Tabs,
                tabs: vec![
                    FieldTab::new("General", vec![FieldDefinition {
                        name: "title".to_string(),
                        ..Default::default()
                    }]),
                    FieldTab::new("Content", vec![FieldDefinition {
                        name: "body".to_string(),
                        ..Default::default()
                    }]),
                ],
                ..Default::default()
            },
        ];

        let mut row = HashMap::new();
        row.insert("title".to_string(), "Hello".to_string());
        row.insert("body".to_string(), "World".to_string());
        set_array_rows(&conn, "posts", "items", "p1", &[row], &sub_fields, None).unwrap();

        let result = find_array_rows(&conn, "posts", "items", "p1", &sub_fields, None).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["title"], "Hello");
        assert_eq!(result[0]["body"], "World");
    }

    #[test]
    fn set_and_find_array_rows_with_row_wrapper() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (id TEXT PRIMARY KEY);
             CREATE TABLE posts_items (
                 id TEXT PRIMARY KEY,
                 parent_id TEXT,
                 _order INTEGER,
                 x TEXT,
                 y TEXT
             );
             INSERT INTO posts (id) VALUES ('p1');",
        ).unwrap();

        let sub_fields = vec![
            FieldDefinition {
                name: "row_wrap".to_string(),
                field_type: FieldType::Row,
                fields: vec![
                    FieldDefinition { name: "x".to_string(), ..Default::default() },
                    FieldDefinition { name: "y".to_string(), ..Default::default() },
                ],
                ..Default::default()
            },
        ];

        let mut row = HashMap::new();
        row.insert("x".to_string(), "10".to_string());
        row.insert("y".to_string(), "20".to_string());
        set_array_rows(&conn, "posts", "items", "p1", &[row], &sub_fields, None).unwrap();

        let result = find_array_rows(&conn, "posts", "items", "p1", &sub_fields, None).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["x"], "10");
        assert_eq!(result[0]["y"], "20");
    }

    #[test]
    fn find_array_rows_empty_sub_fields_returns_only_id() {
        // When there are no sub-fields, set_array_rows returns early (no rows inserted).
        // find_array_rows with empty sub_fields selects only "id" column.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (id TEXT PRIMARY KEY);
             CREATE TABLE posts_items (
                 id TEXT PRIMARY KEY,
                 parent_id TEXT,
                 _order INTEGER
             );
             INSERT INTO posts (id) VALUES ('p1');
             INSERT INTO posts_items (id, parent_id, _order) VALUES ('item1', 'p1', 0);",
        ).unwrap();

        let result = find_array_rows(&conn, "posts", "items", "p1", &[], None).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["id"], "item1");
    }
}
