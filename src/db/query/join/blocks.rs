//! Blocks field join table operations.

use anyhow::Result;
use serde_json::{Map, Value};

use crate::db::query::helpers::join_table;
use crate::db::{DbConnection, DbValue};

use super::helpers::delete_junction_rows;

/// Set block rows for a blocks field join table.
/// Deletes all existing rows for the parent and inserts new ones with nanoid + _order.
/// When `locale` is Some, scopes the DELETE to that locale and includes `_locale` in INSERT.
pub fn set_block_rows(
    conn: &dyn DbConnection,
    collection: &str,
    field_name: &str,
    parent_id: &str,
    rows: &[Value],
    locale: Option<&str>,
) -> Result<()> {
    let table_name = join_table(collection, field_name);

    delete_junction_rows(conn, &table_name, parent_id, locale)?;

    if rows.is_empty() {
        return Ok(());
    }

    if let Some(loc) = locale {
        let (p1, p2, p3, p4, p5, p6) = (
            conn.placeholder(1),
            conn.placeholder(2),
            conn.placeholder(3),
            conn.placeholder(4),
            conn.placeholder(5),
            conn.placeholder(6),
        );
        let sql = format!(
            "INSERT INTO \"{}\" (id, parent_id, _order, _block_type, data, _locale) VALUES ({p1}, {p2}, {p3}, {p4}, {p5}, {p6})",
            table_name
        );
        for (order, row) in rows.iter().enumerate() {
            let id = nanoid::nanoid!();
            let block_type = row
                .get("_block_type")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    anyhow::anyhow!("Block row at index {} is missing '_block_type'", order)
                })?
                .to_string();
            let mut data_map = match row.as_object() {
                Some(m) => m.clone(),
                None => Map::new(),
            };
            data_map.remove("_block_type");
            data_map.remove("id");
            let data_json = Value::Object(data_map).to_string();
            conn.execute(
                &sql,
                &[
                    DbValue::Text(id),
                    DbValue::Text(parent_id.to_string()),
                    DbValue::Integer(order as i64),
                    DbValue::Text(block_type),
                    DbValue::Text(data_json),
                    DbValue::Text(loc.to_string()),
                ],
            )?;
        }
    } else {
        let (p1, p2, p3, p4, p5) = (
            conn.placeholder(1),
            conn.placeholder(2),
            conn.placeholder(3),
            conn.placeholder(4),
            conn.placeholder(5),
        );
        let sql = format!(
            "INSERT INTO \"{}\" (id, parent_id, _order, _block_type, data) VALUES ({p1}, {p2}, {p3}, {p4}, {p5})",
            table_name
        );
        for (order, row) in rows.iter().enumerate() {
            let id = nanoid::nanoid!();
            let block_type = row
                .get("_block_type")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    anyhow::anyhow!("Block row at index {} is missing '_block_type'", order)
                })?
                .to_string();
            let mut data_map = match row.as_object() {
                Some(m) => m.clone(),
                None => Map::new(),
            };
            data_map.remove("_block_type");
            data_map.remove("id");
            let data_json = Value::Object(data_map).to_string();
            conn.execute(
                &sql,
                &[
                    DbValue::Text(id),
                    DbValue::Text(parent_id.to_string()),
                    DbValue::Integer(order as i64),
                    DbValue::Text(block_type),
                    DbValue::Text(data_json),
                ],
            )?;
        }
    }
    Ok(())
}

/// Find block rows for a blocks field join table, ordered.
/// When `locale` is Some, filters by `_locale`.
pub fn find_block_rows(
    conn: &dyn DbConnection,
    collection: &str,
    field_name: &str,
    parent_id: &str,
    locale: Option<&str>,
) -> Result<Vec<Value>> {
    let table_name = join_table(collection, field_name);
    let (sql, params) = if let Some(loc) = locale {
        let (p1, p2) = (conn.placeholder(1), conn.placeholder(2));
        (
            format!(
                "SELECT id, _block_type, data FROM \"{}\" WHERE parent_id = {p1} AND _locale = {p2} ORDER BY _order",
                table_name
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
                "SELECT id, _block_type, data FROM \"{}\" WHERE parent_id = {p1} ORDER BY _order",
                table_name
            ),
            vec![DbValue::Text(parent_id.to_string())],
        )
    };

    let db_rows = conn.query_all(&sql, &params)?;
    let result = db_rows
        .into_iter()
        .filter_map(|row| {
            let id = row.get_value(0).cloned()?;
            let block_type = row.get_value(1).cloned()?;
            let data_raw = row.get_value(2).cloned()?;

            let id_str = if let DbValue::Text(s) = id {
                s
            } else {
                return None;
            };
            let bt_str = if let DbValue::Text(s) = block_type {
                s
            } else {
                return None;
            };
            let data_str = if let DbValue::Text(s) = data_raw {
                s
            } else {
                String::new()
            };

            let mut map = match serde_json::from_str::<Value>(&data_str) {
                Ok(Value::Object(m)) => m,
                _ => Map::new(),
            };
            map.insert("id".to_string(), Value::String(id_str));
            map.insert("_block_type".to_string(), Value::String(bt_str));
            Some(Value::Object(map))
        })
        .collect();
    Ok(result)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::config::CrapConfig;
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

    fn setup_blocks_db() -> (TempDir, BoxedConnection) {
        setup_conn(
            "CREATE TABLE posts (id TEXT PRIMARY KEY);
             CREATE TABLE posts_content (
                 id TEXT PRIMARY KEY,
                 parent_id TEXT,
                 _order INTEGER,
                 _block_type TEXT,
                 data TEXT
             );
             INSERT INTO posts (id) VALUES ('p1');",
        )
    }

    // ── set_block_rows + find_block_rows ─────────────────────────────────────

    #[test]
    fn set_and_find_block_rows() {
        let (_dir, conn) = setup_blocks_db();
        let blocks = vec![
            json!({"_block_type": "paragraph", "text": "Hello world"}),
            json!({"_block_type": "image", "url": "/img/photo.jpg", "alt": "A photo"}),
        ];
        set_block_rows(&conn, "posts", "content", "p1", &blocks, None).unwrap();

        let found = find_block_rows(&conn, "posts", "content", "p1", None).unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(found[0]["_block_type"], "paragraph");
        assert_eq!(found[0]["text"], "Hello world");
        assert_eq!(found[1]["_block_type"], "image");
        assert_eq!(found[1]["url"], "/img/photo.jpg");
        assert_eq!(found[1]["alt"], "A photo");
        assert!(found[0]["id"].as_str().is_some(), "Block should have an id");
        assert!(found[1]["id"].as_str().is_some(), "Block should have an id");
    }

    #[test]
    fn replace_block_rows() {
        let (_dir, conn) = setup_blocks_db();
        let blocks_old = vec![json!({"_block_type": "paragraph", "text": "Old text"})];
        set_block_rows(&conn, "posts", "content", "p1", &blocks_old, None).unwrap();

        let blocks_new = vec![json!({"_block_type": "heading", "level": 1, "text": "New heading"})];
        set_block_rows(&conn, "posts", "content", "p1", &blocks_new, None).unwrap();

        let found = find_block_rows(&conn, "posts", "content", "p1", None).unwrap();
        assert_eq!(found.len(), 1, "Old blocks should be replaced");
        assert_eq!(found[0]["_block_type"], "heading");
        assert_eq!(found[0]["text"], "New heading");
    }

    #[test]
    fn set_and_find_block_rows_with_locale() {
        let (_dir, conn) = setup_conn(
            "CREATE TABLE posts_content (
                id TEXT PRIMARY KEY,
                parent_id TEXT,
                _order INTEGER,
                _block_type TEXT,
                data TEXT,
                _locale TEXT
            );",
        );

        let blocks_en = vec![json!({"_block_type": "text", "body": "Hello"})];
        set_block_rows(&conn, "posts", "content", "p1", &blocks_en, Some("en")).unwrap();

        let blocks_de = vec![json!({"_block_type": "text", "body": "Hallo"})];
        set_block_rows(&conn, "posts", "content", "p1", &blocks_de, Some("de")).unwrap();

        let en = find_block_rows(&conn, "posts", "content", "p1", Some("en")).unwrap();
        assert_eq!(en.len(), 1);
        assert_eq!(en[0]["body"], "Hello");

        let de = find_block_rows(&conn, "posts", "content", "p1", Some("de")).unwrap();
        assert_eq!(de.len(), 1);
        assert_eq!(de[0]["body"], "Hallo");
    }

    #[test]
    fn set_block_rows_missing_block_type_errors() {
        let (_dir, conn) = setup_blocks_db();
        let blocks = vec![json!({"text": "no block type here"})];
        let result = set_block_rows(&conn, "posts", "content", "p1", &blocks, None);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("missing '_block_type'"),
            "Error should mention missing _block_type, got: {msg}"
        );
    }

    #[test]
    fn set_block_rows_missing_block_type_errors_with_locale() {
        let (_dir, conn) = setup_conn(
            "CREATE TABLE posts_content (
                id TEXT PRIMARY KEY,
                parent_id TEXT,
                _order INTEGER,
                _block_type TEXT,
                data TEXT,
                _locale TEXT
            );",
        );
        let blocks = vec![json!({"text": "no block type"})];
        let result = set_block_rows(&conn, "posts", "content", "p1", &blocks, Some("en"));
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("missing '_block_type'"),
            "Error should mention missing _block_type, got: {msg}"
        );
    }

    #[test]
    fn set_block_rows_empty_clears_locale() {
        let (_dir, conn) = setup_conn(
            "CREATE TABLE posts_content (
                id TEXT PRIMARY KEY,
                parent_id TEXT,
                _order INTEGER,
                _block_type TEXT,
                data TEXT,
                _locale TEXT
            );",
        );

        let blocks = vec![json!({"_block_type": "text", "body": "Hi"})];
        set_block_rows(&conn, "posts", "content", "p1", &blocks, Some("en")).unwrap();

        // Clearing with empty slice should remove only the en locale rows
        set_block_rows(&conn, "posts", "content", "p1", &[], Some("en")).unwrap();
        let en = find_block_rows(&conn, "posts", "content", "p1", Some("en")).unwrap();
        assert!(en.is_empty());
    }
}
