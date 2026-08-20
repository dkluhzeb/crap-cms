//! Array field join table operations.

use anyhow::Result;
use serde_json::{Map, Value, json};
use std::collections::HashMap;

use crate::core::{FieldDefinition, FieldType, field::flatten_array_sub_fields};
use crate::db::{
    DbConnection, DbRow, DbValue,
    query::{
        coerce_json_value,
        helpers::{coerce_date_value_json, join_table, tz_column},
    },
};

use super::helpers::{delete_junction_rows, select_junction_rows};

/// Set array rows for an array field join table.
/// Deletes all existing rows for the parent and inserts new ones with nanoid + _order.
/// When `locale` is Some, scopes the DELETE to that locale and includes `_locale` in INSERT.
///
/// # Errors
///
/// Returns a backend error if the DELETE or any per-row INSERT fails.
pub fn set_array_rows(
    conn: &dyn DbConnection,
    collection: &str,
    field_name: &str,
    parent_id: &str,
    rows: &[HashMap<String, Value>],
    sub_fields: &[FieldDefinition],
    locale: Option<&str>,
) -> Result<()> {
    let table_name = join_table(collection, field_name);

    delete_junction_rows(conn, &table_name, parent_id, locale)?;

    let flat_subs = flatten_array_sub_fields(sub_fields);

    if rows.is_empty() || flat_subs.is_empty() {
        return Ok(());
    }

    // Build column list from flattened sub-fields, including _tz companions
    let mut col_names: Vec<String> = Vec::new();
    for sf in &flat_subs {
        col_names.push(sf.name.clone());
        if sf.field_type == FieldType::Date && sf.timezone {
            col_names.push(tz_column(&sf.name));
        }
    }

    let col_list = col_names.join(", ");
    let (all_cols, placeholders) = if locale.is_some() {
        let all_cols = format!("id, parent_id, _order, _locale, {col_list}");
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
        let all_cols = format!("id, parent_id, _order, {col_list}");
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
    let sql = format!("INSERT INTO \"{table_name}\" ({all_cols}) VALUES ({placeholders})");

    for (order, row) in rows.iter().enumerate() {
        let id = nanoid::nanoid!();
        // Row indices saturate at i64::MAX for the unreachable case of >9.2e18
        // rows — it'd only affect sort key ordering at that point.
        let order_i64 = i64::try_from(order).unwrap_or(i64::MAX);
        let mut params: Vec<DbValue> = vec![
            DbValue::Text(id),
            DbValue::Text(parent_id.to_string()),
            DbValue::Integer(order_i64),
        ];

        if let Some(loc) = locale {
            params.push(DbValue::Text(loc.to_string()));
        }

        for sf in &flat_subs {
            let value = row.get(&sf.name).cloned().unwrap_or(Value::Null);

            let db_val = if sf.field_type == FieldType::Date && sf.timezone {
                let tz_key = tz_column(&sf.name);
                coerce_date_value_json(
                    &sf.field_type,
                    &value,
                    row.get(&tz_key).and_then(Value::as_str),
                )
            } else {
                coerce_json_value(&sf.field_type, &value)
            };
            params.push(db_val);

            // Push timezone companion value
            if sf.field_type == FieldType::Date && sf.timezone {
                let tz_key = tz_column(&sf.name);
                let tz_val = row.get(&tz_key).and_then(Value::as_str).unwrap_or("");
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
///
/// # Errors
///
/// Returns a backend error if the SELECT fails.
pub fn find_array_rows(
    conn: &dyn DbConnection,
    collection: &str,
    field_name: &str,
    parent_id: &str,
    sub_fields: &[FieldDefinition],
    locale: Option<&str>,
) -> Result<Vec<Value>> {
    let table_name = join_table(collection, field_name);
    let flat_subs = flatten_array_sub_fields(sub_fields);

    // Build SELECT column list including _tz companions
    let mut select_col_names: Vec<String> = Vec::new();
    for sf in &flat_subs {
        select_col_names.push(sf.name.clone());
        if sf.field_type == FieldType::Date && sf.timezone {
            select_col_names.push(tz_column(&sf.name));
        }
    }
    let select_cols = if select_col_names.is_empty() {
        "id".to_string()
    } else {
        format!("id, {}", select_col_names.join(", "))
    };
    let (sql, params) = select_junction_rows(conn, &table_name, &select_cols, parent_id, locale);

    let db_rows = conn.query_all(&sql, &params)?;
    let mut result = Vec::with_capacity(db_rows.len());

    for db_row in &db_rows {
        let mut map = reconstruct_array_row(db_row, &flat_subs, 1);

        if let Some(DbValue::Text(s)) = db_row.get_value(0) {
            map.insert("id".to_string(), Value::String(s.clone()));
        }

        result.push(Value::Object(map));
    }
    Ok(result)
}

/// Whether a sub-field column holds JSON that must be parsed on read: any
/// composite (Group/Array/Blocks/layout wrapper/Json) or a has-many
/// relationship/upload (stored as a JSON id array in the column).
fn sub_field_stores_json(sf: &FieldDefinition) -> bool {
    matches!(
        sf.field_type,
        FieldType::Array
            | FieldType::Blocks
            | FieldType::Group
            | FieldType::Row
            | FieldType::Collapsible
            | FieldType::Tabs
            | FieldType::Json
    ) || (matches!(sf.field_type, FieldType::Relationship | FieldType::Upload)
        && sf.relationship.as_ref().is_some_and(|rc| rc.has_many))
}

/// Reconstruct an array-row object from a DB row's sub-field columns, starting
/// at column index `start`. Composite sub-fields (Group/Array/Blocks/layout
/// wrappers/Json) are stored as JSON in TEXT columns and parsed back to
/// structured values, so nested composites at any depth come back ready for a
/// JSON walk. Date+timezone fields read their `_tz` companion column.
///
/// Shared by [`find_array_rows`] (per-parent read) and
/// [`find_all_array_rows_with_parent`] (back-reference scan) so the
/// column→JSON mapping lives in exactly one place.
pub(crate) fn reconstruct_array_row(
    db_row: &DbRow,
    flat_subs: &[&FieldDefinition],
    start: usize,
) -> Map<String, Value> {
    let mut map = Map::new();
    let mut col_idx = start;

    for sf in flat_subs {
        let val = db_row.get_value(col_idx).cloned().unwrap_or(DbValue::Null);
        col_idx += 1;

        let json_val = match val {
            DbValue::Integer(n) => json!(n),
            DbValue::Real(f) => json!(f),
            DbValue::Text(s) if sub_field_stores_json(sf) => {
                // Composite sub-fields (and has-many relationship/upload, which
                // store a JSON id array) keep JSON in a TEXT column — parse it
                // so nested data comes back structured.
                serde_json::from_str(&s).unwrap_or(Value::String(s))
            }
            DbValue::Text(s) => Value::String(s),
            DbValue::Null | DbValue::Blob(_) => Value::Null,
        };
        map.insert(sf.name.clone(), json_val);

        if sf.field_type == FieldType::Date && sf.timezone {
            let tz_val = db_row.get_value(col_idx).cloned().unwrap_or(DbValue::Null);
            col_idx += 1;

            let tz_json = match tz_val {
                DbValue::Text(s) => Value::String(s),
                _ => Value::Null,
            };
            map.insert(tz_column(&sf.name), tz_json);
        }
    }

    map
}

/// Load every row of an array join table as `(parent_id, row_object)`, with
/// composite sub-fields JSON-parsed (see [`reconstruct_array_row`]). Used by
/// the back-reference scanner to walk array rows for nested relationships at
/// any depth (group-in-array, array-in-array, has-many in array) rather than
/// querying a single column. Not locale-scoped: a reference exists regardless
/// of which locale's row holds it.
///
/// # Errors
///
/// Returns a backend error if the SELECT fails.
pub(crate) fn find_all_array_rows_with_parent(
    conn: &dyn DbConnection,
    array_table: &str,
    sub_fields: &[FieldDefinition],
) -> Result<Vec<(String, Map<String, Value>)>> {
    let flat_subs = flatten_array_sub_fields(sub_fields);

    let mut select_col_names: Vec<String> = Vec::new();
    for sf in &flat_subs {
        select_col_names.push(sf.name.clone());
        if sf.field_type == FieldType::Date && sf.timezone {
            select_col_names.push(tz_column(&sf.name));
        }
    }

    let select_cols = if select_col_names.is_empty() {
        "parent_id".to_string()
    } else {
        format!("parent_id, {}", select_col_names.join(", "))
    };
    let sql = format!("SELECT {select_cols} FROM \"{array_table}\"");

    let db_rows = conn.query_all(&sql, &[])?;
    let mut result = Vec::with_capacity(db_rows.len());

    for db_row in &db_rows {
        let Some(DbValue::Text(parent_id)) = db_row.get_value(0) else {
            continue;
        };
        let row = reconstruct_array_row(db_row, &flat_subs, 1);

        result.push((parent_id.clone(), row));
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CrapConfig;
    use crate::core::FieldTab;
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
                ("label".to_string(), json!("Label A")),
                ("value".to_string(), json!("Value A")),
            ]),
            HashMap::from([
                ("label".to_string(), json!("Label B")),
                ("value".to_string(), json!("Value B")),
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
            ("label".to_string(), json!("Old")),
            ("value".to_string(), json!("Old Val")),
        ])];
        set_array_rows(&conn, "posts", "items", "p1", &rows_old, &sub, None).unwrap();

        let rows_new = vec![HashMap::from([
            ("label".to_string(), json!("New")),
            ("value".to_string(), json!("New Val")),
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
            ("label".to_string(), json!("X")),
            ("value".to_string(), json!("Y")),
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
        row.insert("title".to_string(), json!("Hello"));
        row.insert("body".to_string(), json!("World"));
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
        row.insert("x".to_string(), json!("10"));
        row.insert("y".to_string(), json!("20"));
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
            ("event_date".to_string(), json!("2024-01-15T09:00")),
            ("event_date_tz".to_string(), json!("America/New_York")),
            ("label".to_string(), json!("Meeting")),
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
            json!("2024-01-15T09:00"),
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

    // ── Deep nesting round-trips (write → DB → read) ─────────────────

    #[test]
    fn array_in_array_round_trips_as_structured_json() {
        // An array nested inside an array row is stored as JSON in the outer
        // row's column and must come back as a structured array, not a string.
        let (_dir, conn) = setup_conn(
            "CREATE TABLE posts (id TEXT PRIMARY KEY);
             CREATE TABLE posts_outer (
                 id TEXT PRIMARY KEY, parent_id TEXT, _order INTEGER, inner TEXT
             );
             INSERT INTO posts (id) VALUES ('p1');",
        );

        let sub_fields = vec![
            FieldDefinition::builder("inner", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("label", FieldType::Text).build(),
                ])
                .build(),
        ];
        let rows = vec![HashMap::from([(
            "inner".to_string(),
            json!([{ "label": "a" }, { "label": "b" }]),
        )])];

        set_array_rows(&conn, "posts", "outer", "p1", &rows, &sub_fields, None).unwrap();
        let found = find_array_rows(&conn, "posts", "outer", "p1", &sub_fields, None).unwrap();

        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0]["inner"],
            json!([{ "label": "a" }, { "label": "b" }])
        );
    }

    #[test]
    fn has_many_relationship_in_array_round_trips_as_array() {
        // A has-many relationship inside an array row stores its id list as JSON
        // in the column and must read back as an array (not a raw string).
        let (_dir, conn) = setup_conn(
            "CREATE TABLE posts (id TEXT PRIMARY KEY);
             CREATE TABLE posts_rows (
                 id TEXT PRIMARY KEY, parent_id TEXT, _order INTEGER, tags TEXT
             );
             INSERT INTO posts (id) VALUES ('p1');",
        );

        let sub_fields = vec![
            FieldDefinition::builder("tags", FieldType::Relationship)
                .relationship(crate::core::RelationshipConfig::new("tags", true))
                .has_many(true)
                .build(),
        ];
        let rows = vec![HashMap::from([("tags".to_string(), json!(["t1", "t2"]))])];

        set_array_rows(&conn, "posts", "rows", "p1", &rows, &sub_fields, None).unwrap();
        let found = find_array_rows(&conn, "posts", "rows", "p1", &sub_fields, None).unwrap();

        assert_eq!(found.len(), 1);
        assert_eq!(found[0]["tags"], json!(["t1", "t2"]));
    }
}
