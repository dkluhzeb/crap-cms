//! Has-many relationship and polymorphic join table operations.

use anyhow::Result;

/// Set related IDs for a has-many relationship junction table.
/// Deletes all existing rows for the parent and inserts new ones with _order.
/// When `locale` is Some, scopes the DELETE to that locale and includes `_locale` in INSERT.
pub fn set_related_ids(
    conn: &rusqlite::Connection,
    collection: &str,
    field: &str,
    parent_id: &str,
    ids: &[String],
    locale: Option<&str>,
) -> Result<()> {
    let table_name = format!("{}_{}", collection, field);
    if let Some(loc) = locale {
        conn.execute(
            &format!(
                "DELETE FROM {} WHERE parent_id = ?1 AND _locale = ?2",
                table_name
            ),
            rusqlite::params![parent_id, loc],
        )?;
    } else {
        conn.execute(
            &format!("DELETE FROM {} WHERE parent_id = ?1", table_name),
            [parent_id],
        )?;
    }

    if let Some(loc) = locale {
        let sql = format!(
            "INSERT INTO {} (parent_id, related_id, _order, _locale) VALUES (?1, ?2, ?3, ?4)",
            table_name
        );
        let mut stmt = conn.prepare(&sql)?;
        for (i, id) in ids.iter().enumerate() {
            stmt.execute(rusqlite::params![parent_id, id, i as i64, loc])?;
        }
    } else {
        let sql = format!(
            "INSERT INTO {} (parent_id, related_id, _order) VALUES (?1, ?2, ?3)",
            table_name
        );
        let mut stmt = conn.prepare(&sql)?;
        for (i, id) in ids.iter().enumerate() {
            stmt.execute(rusqlite::params![parent_id, id, i as i64])?;
        }
    }
    Ok(())
}

/// Find related IDs for a has-many relationship junction table, ordered.
/// When `locale` is Some, filters by `_locale`.
pub fn find_related_ids(
    conn: &rusqlite::Connection,
    collection: &str,
    field: &str,
    parent_id: &str,
    locale: Option<&str>,
) -> Result<Vec<String>> {
    let table_name = format!("{}_{}", collection, field);
    if let Some(loc) = locale {
        let sql = format!(
            "SELECT related_id FROM {} WHERE parent_id = ?1 AND _locale = ?2 ORDER BY _order",
            table_name
        );
        let mut stmt = conn.prepare(&sql)?;
        let ids: Vec<String> = stmt
            .query_map(rusqlite::params![parent_id, loc], |row| {
                row.get::<_, String>(0)
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(ids)
    } else {
        let sql = format!(
            "SELECT related_id FROM {} WHERE parent_id = ?1 ORDER BY _order",
            table_name
        );
        let mut stmt = conn.prepare(&sql)?;
        let ids: Vec<String> = stmt
            .query_map([parent_id], |row| row.get::<_, String>(0))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(ids)
    }
}

/// Set related items for a polymorphic has-many relationship junction table.
/// Each item is a `(related_collection, related_id)` pair.
/// Deletes all existing rows for the parent and inserts new ones with _order.
pub fn set_polymorphic_related(
    conn: &rusqlite::Connection,
    collection: &str,
    field: &str,
    parent_id: &str,
    items: &[(String, String)],
    locale: Option<&str>,
) -> Result<()> {
    let table_name = format!("{}_{}", collection, field);
    if let Some(loc) = locale {
        conn.execute(
            &format!(
                "DELETE FROM {} WHERE parent_id = ?1 AND _locale = ?2",
                table_name
            ),
            rusqlite::params![parent_id, loc],
        )?;
        let sql = format!(
            "INSERT INTO {} (parent_id, related_id, related_collection, _order, _locale) VALUES (?1, ?2, ?3, ?4, ?5)",
            table_name
        );
        let mut stmt = conn.prepare(&sql)?;
        for (i, (rel_col, rel_id)) in items.iter().enumerate() {
            stmt.execute(rusqlite::params![parent_id, rel_id, rel_col, i as i64, loc])?;
        }
    } else {
        conn.execute(
            &format!("DELETE FROM {} WHERE parent_id = ?1", table_name),
            [parent_id],
        )?;
        let sql = format!(
            "INSERT INTO {} (parent_id, related_id, related_collection, _order) VALUES (?1, ?2, ?3, ?4)",
            table_name
        );
        let mut stmt = conn.prepare(&sql)?;
        for (i, (rel_col, rel_id)) in items.iter().enumerate() {
            stmt.execute(rusqlite::params![parent_id, rel_id, rel_col, i as i64])?;
        }
    }
    Ok(())
}

/// Find related items for a polymorphic has-many relationship junction table.
/// Returns `(related_collection, related_id)` pairs ordered by _order.
pub fn find_polymorphic_related(
    conn: &rusqlite::Connection,
    collection: &str,
    field: &str,
    parent_id: &str,
    locale: Option<&str>,
) -> Result<Vec<(String, String)>> {
    let table_name = format!("{}_{}", collection, field);
    if let Some(loc) = locale {
        let sql = format!(
            "SELECT related_collection, related_id FROM {} WHERE parent_id = ?1 AND _locale = ?2 ORDER BY _order",
            table_name
        );
        let mut stmt = conn.prepare(&sql)?;
        let items: Vec<(String, String)> = stmt
            .query_map(rusqlite::params![parent_id, loc], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(items)
    } else {
        let sql = format!(
            "SELECT related_collection, related_id FROM {} WHERE parent_id = ?1 ORDER BY _order",
            table_name
        );
        let mut stmt = conn.prepare(&sql)?;
        let items: Vec<(String, String)> = stmt
            .query_map([parent_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(items)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn setup_junction_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts_tags (
                parent_id TEXT,
                related_id TEXT,
                _order INTEGER
            );",
        )
        .unwrap();
        conn
    }

    fn setup_polymorphic_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts_refs (
                parent_id TEXT,
                related_id TEXT,
                related_collection TEXT NOT NULL DEFAULT '',
                _order INTEGER,
                PRIMARY KEY (parent_id, related_id, related_collection)
            );",
        )
        .unwrap();
        conn
    }

    // ── set_related_ids + find_related_ids ───────────────────────────────────

    #[test]
    fn set_and_find_related_ids() {
        let conn = setup_junction_db();
        let ids = vec!["t1".to_string(), "t2".to_string(), "t3".to_string()];
        set_related_ids(&conn, "posts", "tags", "p1", &ids, None).unwrap();

        let found = find_related_ids(&conn, "posts", "tags", "p1", None).unwrap();
        assert_eq!(
            found,
            vec!["t1", "t2", "t3"],
            "Should return IDs in insertion order"
        );
    }

    #[test]
    fn replace_related_ids() {
        let conn = setup_junction_db();
        let ids_old = vec!["t1".to_string(), "t2".to_string()];
        set_related_ids(&conn, "posts", "tags", "p1", &ids_old, None).unwrap();

        let ids_new = vec!["t3".to_string(), "t4".to_string()];
        set_related_ids(&conn, "posts", "tags", "p1", &ids_new, None).unwrap();

        let found = find_related_ids(&conn, "posts", "tags", "p1", None).unwrap();
        assert_eq!(
            found,
            vec!["t3", "t4"],
            "Old IDs should be replaced by new ones"
        );
    }

    #[test]
    fn empty_related_ids() {
        let conn = setup_junction_db();
        let ids = vec!["t1".to_string()];
        set_related_ids(&conn, "posts", "tags", "p1", &ids, None).unwrap();
        set_related_ids(&conn, "posts", "tags", "p1", &[], None).unwrap();

        let found = find_related_ids(&conn, "posts", "tags", "p1", None).unwrap();
        assert!(
            found.is_empty(),
            "Should return empty list after setting empty IDs"
        );
    }

    #[test]
    fn set_and_find_related_ids_with_locale() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts_tags (
                parent_id TEXT,
                related_id TEXT,
                _order INTEGER,
                _locale TEXT
            );",
        )
        .unwrap();

        set_related_ids(
            &conn,
            "posts",
            "tags",
            "p1",
            &["t1".to_string(), "t2".to_string()],
            Some("en"),
        )
        .unwrap();
        set_related_ids(
            &conn,
            "posts",
            "tags",
            "p1",
            &["t3".to_string()],
            Some("de"),
        )
        .unwrap();

        let en = find_related_ids(&conn, "posts", "tags", "p1", Some("en")).unwrap();
        assert_eq!(en, vec!["t1", "t2"]);

        let de = find_related_ids(&conn, "posts", "tags", "p1", Some("de")).unwrap();
        assert_eq!(de, vec!["t3"]);

        // Replacing en should not affect de
        set_related_ids(
            &conn,
            "posts",
            "tags",
            "p1",
            &["t4".to_string()],
            Some("en"),
        )
        .unwrap();
        let en = find_related_ids(&conn, "posts", "tags", "p1", Some("en")).unwrap();
        assert_eq!(en, vec!["t4"]);
        let de = find_related_ids(&conn, "posts", "tags", "p1", Some("de")).unwrap();
        assert_eq!(de, vec!["t3"]);
    }

    // ── set_polymorphic_related + find_polymorphic_related ───────────────────

    #[test]
    fn set_and_find_polymorphic_related() {
        let conn = setup_polymorphic_db();
        let items = vec![
            ("articles".to_string(), "a1".to_string()),
            ("pages".to_string(), "pg1".to_string()),
            ("articles".to_string(), "a2".to_string()),
        ];
        set_polymorphic_related(&conn, "posts", "refs", "p1", &items, None).unwrap();

        let found = find_polymorphic_related(&conn, "posts", "refs", "p1", None).unwrap();
        assert_eq!(
            found,
            vec![
                ("articles".to_string(), "a1".to_string()),
                ("pages".to_string(), "pg1".to_string()),
                ("articles".to_string(), "a2".to_string()),
            ]
        );
    }

    #[test]
    fn replace_polymorphic_related() {
        let conn = setup_polymorphic_db();
        let old = vec![("articles".to_string(), "a1".to_string())];
        set_polymorphic_related(&conn, "posts", "refs", "p1", &old, None).unwrap();

        let new_items = vec![
            ("pages".to_string(), "pg1".to_string()),
            ("pages".to_string(), "pg2".to_string()),
        ];
        set_polymorphic_related(&conn, "posts", "refs", "p1", &new_items, None).unwrap();

        let found = find_polymorphic_related(&conn, "posts", "refs", "p1", None).unwrap();
        assert_eq!(
            found,
            vec![
                ("pages".to_string(), "pg1".to_string()),
                ("pages".to_string(), "pg2".to_string()),
            ]
        );
    }

    #[test]
    fn set_and_find_polymorphic_related_with_locale() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts_refs (
                parent_id TEXT,
                related_id TEXT,
                related_collection TEXT NOT NULL DEFAULT '',
                _order INTEGER,
                _locale TEXT
            );",
        )
        .unwrap();

        let items_en = vec![
            ("articles".to_string(), "a1".to_string()),
            ("pages".to_string(), "pg1".to_string()),
        ];
        set_polymorphic_related(&conn, "posts", "refs", "p1", &items_en, Some("en")).unwrap();

        let items_de = vec![("articles".to_string(), "a2".to_string())];
        set_polymorphic_related(&conn, "posts", "refs", "p1", &items_de, Some("de")).unwrap();

        let en = find_polymorphic_related(&conn, "posts", "refs", "p1", Some("en")).unwrap();
        assert_eq!(en.len(), 2);
        assert_eq!(en[0], ("articles".to_string(), "a1".to_string()));

        let de = find_polymorphic_related(&conn, "posts", "refs", "p1", Some("de")).unwrap();
        assert_eq!(de.len(), 1);
        assert_eq!(de[0], ("articles".to_string(), "a2".to_string()));

        // Replacing en should not affect de
        set_polymorphic_related(
            &conn,
            "posts",
            "refs",
            "p1",
            &[("pages".to_string(), "pg2".to_string())],
            Some("en"),
        )
        .unwrap();
        let en2 = find_polymorphic_related(&conn, "posts", "refs", "p1", Some("en")).unwrap();
        assert_eq!(en2.len(), 1);
        assert_eq!(en2[0], ("pages".to_string(), "pg2".to_string()));
        let de2 = find_polymorphic_related(&conn, "posts", "refs", "p1", Some("de")).unwrap();
        assert_eq!(de2.len(), 1, "de locale should be unchanged");
    }
}
