//! Blocks field join table operations.

use anyhow::{Context as _, Result};

/// Set block rows for a blocks field join table.
/// Deletes all existing rows for the parent and inserts new ones with nanoid + _order.
/// When `locale` is Some, scopes the DELETE to that locale and includes `_locale` in INSERT.
pub fn set_block_rows(
    conn: &rusqlite::Connection,
    collection: &str,
    field_name: &str,
    parent_id: &str,
    rows: &[serde_json::Value],
    locale: Option<&str>,
) -> Result<()> {
    let table_name = format!("{}_{}", collection, field_name);
    if let Some(loc) = locale {
        conn.execute(
            &format!(
                "DELETE FROM {} WHERE parent_id = ?1 AND _locale = ?2",
                table_name
            ),
            rusqlite::params![parent_id, loc],
        )
        .with_context(|| format!("Failed to clear blocks table {}", table_name))?;
    } else {
        conn.execute(
            &format!("DELETE FROM {} WHERE parent_id = ?1", table_name),
            [parent_id],
        )
        .with_context(|| format!("Failed to clear blocks table {}", table_name))?;
    }

    if rows.is_empty() {
        return Ok(());
    }

    if let Some(loc) = locale {
        let sql = format!(
            "INSERT INTO {} (id, parent_id, _order, _block_type, data, _locale) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            table_name
        );
        let mut stmt = conn.prepare(&sql)?;
        for (order, row) in rows.iter().enumerate() {
            let id = nanoid::nanoid!();
            let block_type = row
                .get("_block_type")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            let mut data_map = match row.as_object() {
                Some(m) => m.clone(),
                None => serde_json::Map::new(),
            };
            data_map.remove("_block_type");
            data_map.remove("id");
            let data_json = serde_json::Value::Object(data_map).to_string();
            stmt.execute(rusqlite::params![
                id,
                parent_id,
                order as i64,
                block_type,
                data_json,
                loc
            ])?;
        }
    } else {
        let sql = format!(
            "INSERT INTO {} (id, parent_id, _order, _block_type, data) VALUES (?1, ?2, ?3, ?4, ?5)",
            table_name
        );
        let mut stmt = conn.prepare(&sql)?;
        for (order, row) in rows.iter().enumerate() {
            let id = nanoid::nanoid!();
            let block_type = row
                .get("_block_type")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            let mut data_map = match row.as_object() {
                Some(m) => m.clone(),
                None => serde_json::Map::new(),
            };
            data_map.remove("_block_type");
            data_map.remove("id");
            let data_json = serde_json::Value::Object(data_map).to_string();
            stmt.execute(rusqlite::params![
                id,
                parent_id,
                order as i64,
                block_type,
                data_json
            ])?;
        }
    }
    Ok(())
}

/// Find block rows for a blocks field join table, ordered.
/// When `locale` is Some, filters by `_locale`.
pub fn find_block_rows(
    conn: &rusqlite::Connection,
    collection: &str,
    field_name: &str,
    parent_id: &str,
    locale: Option<&str>,
) -> Result<Vec<serde_json::Value>> {
    let table_name = format!("{}_{}", collection, field_name);
    let sql = if locale.is_some() {
        format!(
            "SELECT id, _block_type, data FROM {} WHERE parent_id = ?1 AND _locale = ?2 ORDER BY _order",
            table_name
        )
    } else {
        format!(
            "SELECT id, _block_type, data FROM {} WHERE parent_id = ?1 ORDER BY _order",
            table_name
        )
    };
    let mut stmt = conn.prepare(&sql)?;
    let params: Vec<Box<dyn rusqlite::types::ToSql>> = if let Some(loc) = locale {
        vec![Box::new(parent_id.to_string()), Box::new(loc.to_string())]
    } else {
        vec![Box::new(parent_id.to_string())]
    };
    let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let rows = stmt
        .query_map(rusqlite::params_from_iter(param_refs.iter()), |row| {
            let id: String = row.get(0)?;
            let block_type: String = row.get(1)?;
            let data_json: String = row.get(2)?;
            Ok((id, block_type, data_json))
        })?
        .filter_map(|r| r.ok())
        .map(|(id, block_type, data_json)| {
            let mut map = match serde_json::from_str::<serde_json::Value>(&data_json) {
                Ok(serde_json::Value::Object(m)) => m,
                _ => serde_json::Map::new(),
            };
            map.insert("id".to_string(), serde_json::Value::String(id));
            map.insert(
                "_block_type".to_string(),
                serde_json::Value::String(block_type),
            );
            serde_json::Value::Object(map)
        })
        .collect();
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn setup_blocks_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
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
        .unwrap();
        conn
    }

    // ── set_block_rows + find_block_rows ─────────────────────────────────────

    #[test]
    fn set_and_find_block_rows() {
        let conn = setup_blocks_db();
        let blocks = vec![
            serde_json::json!({"_block_type": "paragraph", "text": "Hello world"}),
            serde_json::json!({"_block_type": "image", "url": "/img/photo.jpg", "alt": "A photo"}),
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
        let conn = setup_blocks_db();
        let blocks_old = vec![serde_json::json!({"_block_type": "paragraph", "text": "Old text"})];
        set_block_rows(&conn, "posts", "content", "p1", &blocks_old, None).unwrap();

        let blocks_new =
            vec![serde_json::json!({"_block_type": "heading", "level": 1, "text": "New heading"})];
        set_block_rows(&conn, "posts", "content", "p1", &blocks_new, None).unwrap();

        let found = find_block_rows(&conn, "posts", "content", "p1", None).unwrap();
        assert_eq!(found.len(), 1, "Old blocks should be replaced");
        assert_eq!(found[0]["_block_type"], "heading");
        assert_eq!(found[0]["text"], "New heading");
    }

    #[test]
    fn set_and_find_block_rows_with_locale() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts_content (
                id TEXT PRIMARY KEY,
                parent_id TEXT,
                _order INTEGER,
                _block_type TEXT,
                data TEXT,
                _locale TEXT
            );",
        )
        .unwrap();

        let blocks_en = vec![serde_json::json!({"_block_type": "text", "body": "Hello"})];
        set_block_rows(&conn, "posts", "content", "p1", &blocks_en, Some("en")).unwrap();

        let blocks_de = vec![serde_json::json!({"_block_type": "text", "body": "Hallo"})];
        set_block_rows(&conn, "posts", "content", "p1", &blocks_de, Some("de")).unwrap();

        let en = find_block_rows(&conn, "posts", "content", "p1", Some("en")).unwrap();
        assert_eq!(en.len(), 1);
        assert_eq!(en[0]["body"], "Hello");

        let de = find_block_rows(&conn, "posts", "content", "p1", Some("de")).unwrap();
        assert_eq!(de.len(), 1);
        assert_eq!(de[0]["body"], "Hallo");
    }

    #[test]
    fn set_block_rows_empty_clears_locale() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts_content (
                id TEXT PRIMARY KEY,
                parent_id TEXT,
                _order INTEGER,
                _block_type TEXT,
                data TEXT,
                _locale TEXT
            );",
        )
        .unwrap();

        let blocks = vec![serde_json::json!({"_block_type": "text", "body": "Hi"})];
        set_block_rows(&conn, "posts", "content", "p1", &blocks, Some("en")).unwrap();

        // Clearing with empty slice should remove only the en locale rows
        set_block_rows(&conn, "posts", "content", "p1", &[], Some("en")).unwrap();
        let en = find_block_rows(&conn, "posts", "content", "p1", Some("en")).unwrap();
        assert!(en.is_empty());
    }
}
