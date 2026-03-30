//! Array field join table operations.

use anyhow::Result;
use serde_json::{Map, Value, json};
use std::collections::HashMap;

use crate::core::{FieldDefinition, FieldType, field::flatten_array_sub_fields};
use crate::db::{
    DbConnection, DbValue,
    query::{coerce_value, helpers::normalize_date_with_timezone},
};

/// Set array rows for an array field join table.
/// Deletes all existing rows for the parent and inserts new ones with nanoid + _order.
/// When `locale` is Some, scopes the DELETE to that locale and includes `_locale` in INSERT.
pub fn set_array_rows(
    conn: &dyn DbConnection,
    collection: &str,
    field_name: &str,
    parent_id: &str,
    rows: &[HashMap<String, String>],
    sub_fields: &[FieldDefinition],
    locale: Option<&str>,
) -> Result<()> {
    let table_name = format!("{}_{}", collection, field_name);

    if let Some(loc) = locale {
        let (p1, p2) = (conn.placeholder(1), conn.placeholder(2));
        conn.execute(
            &format!(
                "DELETE FROM \"{}\" WHERE parent_id = {p1} AND _locale = {p2}",
                table_name
            ),
            &[
                DbValue::Text(parent_id.to_string()),
                DbValue::Text(loc.to_string()),
            ],
        )?;
    } else {
        let p1 = conn.placeholder(1);
        conn.execute(
            &format!("DELETE FROM \"{}\" WHERE parent_id = {p1}", table_name),
            &[DbValue::Text(parent_id.to_string())],
        )?;
    }

    let flat_subs = flatten_array_sub_fields(sub_fields);

    if rows.is_empty() || flat_subs.is_empty() {
        return Ok(());
    }

    // Build column list from flattened sub-fields, including _tz companions
    let mut col_names: Vec<String> = Vec::new();
    for sf in &flat_subs {
        col_names.push(sf.name.clone());
        if sf.field_type == FieldType::Date && sf.timezone {
            col_names.push(format!("{}_tz", sf.name));
        }
    }

    let col_list = col_names.join(", ");
    let (all_cols, placeholders) = if locale.is_some() {
        let all_cols = format!("id, parent_id, _order, _locale, {}", col_list);
        let placeholders = format!(
            "{}, {}, {}, {}, {}",
            conn.placeholder(1),
            conn.placeholder(2),
            conn.placeholder(3),
            conn.placeholder(4),
            (5..5 + col_names.len())
                .map(|i| conn.placeholder(i))
                .collect::<Vec<_>>()
                .join(", ")
        );
        (all_cols, placeholders)
    } else {
        let all_cols = format!("id, parent_id, _order, {}", col_list);
        let placeholders = format!(
            "{}, {}, {}, {}",
            conn.placeholder(1),
            conn.placeholder(2),
            conn.placeholder(3),
            (4..4 + col_names.len())
                .map(|i| conn.placeholder(i))
                .collect::<Vec<_>>()
                .join(", ")
        );
        (all_cols, placeholders)
    };
    let sql = format!(
        "INSERT INTO \"{}\" ({}) VALUES ({})",
        table_name, all_cols, placeholders
    );

    for (order, row) in rows.iter().enumerate() {
        let id = nanoid::nanoid!();
        let mut params: Vec<DbValue> = vec![
            DbValue::Text(id),
            DbValue::Text(parent_id.to_string()),
            DbValue::Integer(order as i64),
        ];

        if let Some(loc) = locale {
            params.push(DbValue::Text(loc.to_string()));
        }

        for sf in &flat_subs {
            let value = row.get(&sf.name).cloned().unwrap_or_default();

            // For Date fields with timezone, use timezone-aware normalization
            let db_val = if sf.field_type == FieldType::Date && sf.timezone {
                let tz_key = format!("{}_tz", sf.name);
                if let Some(tz) = row.get(&tz_key).filter(|s| !s.is_empty()) {
                    if value.is_empty() {
                        DbValue::Null
                    } else {
                        match normalize_date_with_timezone(&value, tz) {
                            Ok(normalized) => DbValue::Text(normalized),
                            Err(_) => coerce_value(&sf.field_type, &value),
                        }
                    }
                } else {
                    coerce_value(&sf.field_type, &value)
                }
            } else {
                coerce_value(&sf.field_type, &value)
            };
            params.push(db_val);

            // Push timezone companion value
            if sf.field_type == FieldType::Date && sf.timezone {
                let tz_key = format!("{}_tz", sf.name);
                let tz_val = row.get(&tz_key).map(|s| s.as_str()).unwrap_or("");
                params.push(if tz_val.is_empty() {
                    DbValue::Null
                } else {
                    DbValue::Text(tz_val.to_string())
                });
            }
        }

        conn.execute(&sql, &params)?;
    }
    Ok(())
}

/// Find array rows for an array field join table, ordered.
/// When `locale` is Some, filters by `_locale`.
pub fn find_array_rows(
    conn: &dyn DbConnection,
    collection: &str,
    field_name: &str,
    parent_id: &str,
    sub_fields: &[FieldDefinition],
    locale: Option<&str>,
) -> Result<Vec<Value>> {
    let table_name = format!("{}_{}", collection, field_name);
    let flat_subs = flatten_array_sub_fields(sub_fields);

    // Build SELECT column list including _tz companions
    let mut select_col_names: Vec<String> = Vec::new();
    for sf in &flat_subs {
        select_col_names.push(sf.name.clone());
        if sf.field_type == FieldType::Date && sf.timezone {
            select_col_names.push(format!("{}_tz", sf.name));
        }
    }
    let select_cols = if select_col_names.is_empty() {
        "id".to_string()
    } else {
        format!("id, {}", select_col_names.join(", "))
    };
    let (sql, params) = if let Some(loc) = locale {
        let (p1, p2) = (conn.placeholder(1), conn.placeholder(2));
        (
            format!(
                "SELECT {} FROM \"{}\" WHERE parent_id = {p1} AND _locale = {p2} ORDER BY _order",
                select_cols, table_name
            ),
            vec![
                DbValue::Text(parent_id.to_string()),
                DbValue::Text(loc.to_string()),
            ],
        )
    } else {
        let p1 = conn.placeholder(1);
        (
            format!(
                "SELECT {} FROM \"{}\" WHERE parent_id = {p1} ORDER BY _order",
                select_cols, table_name
            ),
            vec![DbValue::Text(parent_id.to_string())],
        )
    };

    let db_rows = conn.query_all(&sql, &params)?;
    let mut result = Vec::with_capacity(db_rows.len());

    for db_row in &db_rows {
        let mut map = Map::new();
        let id = db_row.get_value(0).cloned().unwrap_or(DbValue::Null);
        if let DbValue::Text(s) = id {
            map.insert("id".to_string(), Value::String(s));
        }

        let mut col_idx = 1; // starts after "id"
        for sf in &flat_subs {
            let val = db_row.get_value(col_idx).cloned().unwrap_or(DbValue::Null);
            col_idx += 1;

            let json_val = match val {
                DbValue::Null => Value::Null,
                DbValue::Integer(n) => json!(n),
                DbValue::Real(f) => json!(f),
                DbValue::Text(s) => {
                    // Composite sub-fields store JSON in TEXT columns —
                    // attempt to parse so nested data comes back structured.
                    match sf.field_type {
                        FieldType::Array
                        | FieldType::Blocks
                        | FieldType::Group
                        | FieldType::Row
                        | FieldType::Collapsible
                        | FieldType::Tabs
                        | FieldType::Json => serde_json::from_str(&s).unwrap_or(Value::String(s)),
                        _ => Value::String(s),
                    }
                }
                DbValue::Blob(_) => Value::Null,
            };
            map.insert(sf.name.clone(), json_val);

            // Read timezone companion column
            if sf.field_type == FieldType::Date && sf.timezone {
                let tz_val = db_row.get_value(col_idx).cloned().unwrap_or(DbValue::Null);
                col_idx += 1;

                let tz_json = match tz_val {
                    DbValue::Text(s) => Value::String(s),
                    _ => Value::Null,
                };
                map.insert(format!("{}_tz", sf.name), tz_json);
            }
        }

        result.push(Value::Object(map));
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CrapConfig;
    use crate::core::field::FieldTab;
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

    fn setup_array_db() -> (TempDir, BoxedConnection) {
        setup_conn(
            "CREATE TABLE posts (id TEXT PRIMARY KEY);
             CREATE TABLE posts_items (
                 id TEXT PRIMARY KEY,
                 parent_id TEXT,
                 _order INTEGER,
                 label TEXT,
                 value TEXT
             );
             INSERT INTO posts (id) VALUES ('p1');",
        )
    }

    fn array_sub_fields() -> Vec<FieldDefinition> {
        vec![
            FieldDefinition::builder("label", FieldType::Text).build(),
            FieldDefinition::builder("value", FieldType::Text).build(),
        ]
    }

    // ── set_array_rows + find_array_rows ─────────────────────────────────────

    #[test]
    fn set_and_find_array_rows() {
        let (_dir, conn) = setup_array_db();
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
        let (_dir, conn) = setup_array_db();
        let sub = array_sub_fields();
        let rows_old = vec![HashMap::from([
            ("label".to_string(), "Old".to_string()),
            ("value".to_string(), "Old Val".to_string()),
        ])];
        set_array_rows(&conn, "posts", "items", "p1", &rows_old, &sub, None).unwrap();

        let rows_new = vec![HashMap::from([
            ("label".to_string(), "New".to_string()),
            ("value".to_string(), "New Val".to_string()),
        ])];
        set_array_rows(&conn, "posts", "items", "p1", &rows_new, &sub, None).unwrap();

        let found = find_array_rows(&conn, "posts", "items", "p1", &sub, None).unwrap();
        assert_eq!(found.len(), 1, "Old rows should be replaced");
        assert_eq!(found[0]["label"], "New");
        assert_eq!(found[0]["value"], "New Val");
    }

    #[test]
    fn empty_array_rows() {
        let (_dir, conn) = setup_array_db();
        let sub = array_sub_fields();
        let rows = vec![HashMap::from([
            ("label".to_string(), "X".to_string()),
            ("value".to_string(), "Y".to_string()),
        ])];
        set_array_rows(&conn, "posts", "items", "p1", &rows, &sub, None).unwrap();
        set_array_rows(&conn, "posts", "items", "p1", &[], &sub, None).unwrap();

        let found = find_array_rows(&conn, "posts", "items", "p1", &sub, None).unwrap();
        assert!(
            found.is_empty(),
            "Should return empty after setting empty rows"
        );
    }

    #[test]
    fn set_and_find_array_rows_with_tabs() {
        let (_dir, conn) = setup_conn(
            "CREATE TABLE posts (id TEXT PRIMARY KEY);
             CREATE TABLE posts_items (
                 id TEXT PRIMARY KEY,
                 parent_id TEXT,
                 _order INTEGER,
                 title TEXT,
                 body TEXT
             );
             INSERT INTO posts (id) VALUES ('p1');",
        );

        // Sub-fields wrapped in Tabs
        let sub_fields = vec![
            FieldDefinition::builder("layout", FieldType::Tabs)
                .tabs(vec![
                    FieldTab::new(
                        "General",
                        vec![FieldDefinition::builder("title", FieldType::Text).build()],
                    ),
                    FieldTab::new(
                        "Content",
                        vec![FieldDefinition::builder("body", FieldType::Text).build()],
                    ),
                ])
                .build(),
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
        let (_dir, conn) = setup_conn(
            "CREATE TABLE posts (id TEXT PRIMARY KEY);
             CREATE TABLE posts_items (
                 id TEXT PRIMARY KEY,
                 parent_id TEXT,
                 _order INTEGER,
                 x TEXT,
                 y TEXT
             );
             INSERT INTO posts (id) VALUES ('p1');",
        );

        let sub_fields = vec![
            FieldDefinition::builder("row_wrap", FieldType::Row)
                .fields(vec![
                    FieldDefinition::builder("x", FieldType::Text).build(),
                    FieldDefinition::builder("y", FieldType::Text).build(),
                ])
                .build(),
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
        let (_dir, conn) = setup_conn(
            "CREATE TABLE posts (id TEXT PRIMARY KEY);
             CREATE TABLE posts_items (
                 id TEXT PRIMARY KEY,
                 parent_id TEXT,
                 _order INTEGER
             );
             INSERT INTO posts (id) VALUES ('p1');
             INSERT INTO posts_items (id, parent_id, _order) VALUES ('item1', 'p1', 0);",
        );

        let result = find_array_rows(&conn, "posts", "items", "p1", &[], None).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["id"], "item1");
    }

    // ── Timezone companion tests ─────────────────────────────────────

    #[test]
    fn set_and_find_array_rows_with_date_timezone() {
        let (_dir, conn) = setup_conn(
            "CREATE TABLE posts (id TEXT PRIMARY KEY);
             CREATE TABLE posts_schedule (
                 id TEXT PRIMARY KEY,
                 parent_id TEXT,
                 _order INTEGER,
                 event_date TEXT,
                 event_date_tz TEXT,
                 label TEXT
             );
             INSERT INTO posts (id) VALUES ('p1');",
        );

        let sub_fields = vec![
            FieldDefinition::builder("event_date", FieldType::Date)
                .timezone(true)
                .build(),
            FieldDefinition::builder("label", FieldType::Text).build(),
        ];

        let rows = vec![HashMap::from([
            ("event_date".to_string(), "2024-01-15T09:00".to_string()),
            ("event_date_tz".to_string(), "America/New_York".to_string()),
            ("label".to_string(), "Meeting".to_string()),
        ])];

        set_array_rows(&conn, "posts", "schedule", "p1", &rows, &sub_fields, None).unwrap();

        let found = find_array_rows(&conn, "posts", "schedule", "p1", &sub_fields, None).unwrap();
        assert_eq!(found.len(), 1);

        // 9am EST = 2pm UTC
        assert_eq!(found[0]["event_date"], "2024-01-15T14:00:00.000Z");
        assert_eq!(found[0]["event_date_tz"], "America/New_York");
        assert_eq!(found[0]["label"], "Meeting");
    }

    #[test]
    fn set_array_rows_date_tz_without_tz_value() {
        let (_dir, conn) = setup_conn(
            "CREATE TABLE posts (id TEXT PRIMARY KEY);
             CREATE TABLE posts_schedule (
                 id TEXT PRIMARY KEY,
                 parent_id TEXT,
                 _order INTEGER,
                 event_date TEXT,
                 event_date_tz TEXT
             );
             INSERT INTO posts (id) VALUES ('p1');",
        );

        let sub_fields = vec![
            FieldDefinition::builder("event_date", FieldType::Date)
                .timezone(true)
                .build(),
        ];

        let rows = vec![HashMap::from([(
            "event_date".to_string(),
            "2024-01-15T09:00".to_string(),
        )])];

        set_array_rows(&conn, "posts", "schedule", "p1", &rows, &sub_fields, None).unwrap();

        let found = find_array_rows(&conn, "posts", "schedule", "p1", &sub_fields, None).unwrap();
        assert_eq!(found.len(), 1);

        // No timezone provided — falls back to treat as UTC
        assert_eq!(found[0]["event_date"], "2024-01-15T09:00:00.000Z");
        assert!(
            found[0]["event_date_tz"].is_null(),
            "tz should be null when not provided"
        );
    }
}
