//! Has-many relationship and polymorphic join table operations.

use anyhow::Result;

use crate::db::{DbConnection, DbValue};

/// Set related IDs for a has-many relationship junction table.
/// Deletes all existing rows for the parent and inserts new ones with _order.
/// When `locale` is Some, scopes the DELETE to that locale and includes `_locale` in INSERT.
pub fn set_related_ids(
    conn: &dyn DbConnection,
    collection: &str,
    field: &str,
    parent_id: &str,
    ids: &[String],
    locale: Option<&str>,
) -> Result<()> {
    let table_name = format!("{}_{}", collection, field);

    if let Some(loc) = locale {
        let (p1, p2) = (conn.placeholder(1), conn.placeholder(2));
        conn.execute(
            &format!(
                "DELETE FROM {} WHERE parent_id = {p1} AND _locale = {p2}",
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
            &format!("DELETE FROM {} WHERE parent_id = {p1}", table_name),
            &[DbValue::Text(parent_id.to_string())],
        )?;
    }

    if let Some(loc) = locale {
        let (p1, p2, p3, p4) = (
            conn.placeholder(1),
            conn.placeholder(2),
            conn.placeholder(3),
            conn.placeholder(4),
        );
        let sql = format!(
            "INSERT INTO {} (parent_id, related_id, _order, _locale) VALUES ({p1}, {p2}, {p3}, {p4})",
            table_name
        );
        for (i, id) in ids.iter().enumerate() {
            conn.execute(
                &sql,
                &[
                    DbValue::Text(parent_id.to_string()),
                    DbValue::Text(id.clone()),
                    DbValue::Integer(i as i64),
                    DbValue::Text(loc.to_string()),
                ],
            )?;
        }
    } else {
        let (p1, p2, p3) = (
            conn.placeholder(1),
            conn.placeholder(2),
            conn.placeholder(3),
        );
        let sql = format!(
            "INSERT INTO {} (parent_id, related_id, _order) VALUES ({p1}, {p2}, {p3})",
            table_name
        );
        for (i, id) in ids.iter().enumerate() {
            conn.execute(
                &sql,
                &[
                    DbValue::Text(parent_id.to_string()),
                    DbValue::Text(id.clone()),
                    DbValue::Integer(i as i64),
                ],
            )?;
        }
    }
    Ok(())
}

/// Find related IDs for a has-many relationship junction table, ordered.
/// When `locale` is Some, filters by `_locale`.
pub fn find_related_ids(
    conn: &dyn DbConnection,
    collection: &str,
    field: &str,
    parent_id: &str,
    locale: Option<&str>,
) -> Result<Vec<String>> {
    let table_name = format!("{}_{}", collection, field);

    let (sql, params) = if let Some(loc) = locale {
        let (p1, p2) = (conn.placeholder(1), conn.placeholder(2));
        (
            format!(
                "SELECT related_id FROM {} WHERE parent_id = {p1} AND _locale = {p2} ORDER BY _order",
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
                "SELECT related_id FROM {} WHERE parent_id = {p1} ORDER BY _order",
                table_name
            ),
            vec![DbValue::Text(parent_id.to_string())],
        )
    };

    let rows = conn.query_all(&sql, &params)?;
    let ids = rows
        .into_iter()
        .filter_map(|row| {
            if let Some(DbValue::Text(s)) = row.get_value(0) {
                Some(s.clone())
            } else {
                None
            }
        })
        .collect();
    Ok(ids)
}

/// Set related items for a polymorphic has-many relationship junction table.
/// Each item is a `(related_collection, related_id)` pair.
/// Deletes all existing rows for the parent and inserts new ones with _order.
pub fn set_polymorphic_related(
    conn: &dyn DbConnection,
    collection: &str,
    field: &str,
    parent_id: &str,
    items: &[(String, String)],
    locale: Option<&str>,
) -> Result<()> {
    let table_name = format!("{}_{}", collection, field);

    if let Some(loc) = locale {
        let (p1, p2) = (conn.placeholder(1), conn.placeholder(2));
        conn.execute(
            &format!(
                "DELETE FROM {} WHERE parent_id = {p1} AND _locale = {p2}",
                table_name
            ),
            &[
                DbValue::Text(parent_id.to_string()),
                DbValue::Text(loc.to_string()),
            ],
        )?;
        let (p1, p2, p3, p4, p5) = (
            conn.placeholder(1),
            conn.placeholder(2),
            conn.placeholder(3),
            conn.placeholder(4),
            conn.placeholder(5),
        );
        let sql = format!(
            "INSERT INTO {} (parent_id, related_id, related_collection, _order, _locale) VALUES ({p1}, {p2}, {p3}, {p4}, {p5})",
            table_name
        );
        for (i, (rel_col, rel_id)) in items.iter().enumerate() {
            conn.execute(
                &sql,
                &[
                    DbValue::Text(parent_id.to_string()),
                    DbValue::Text(rel_id.clone()),
                    DbValue::Text(rel_col.clone()),
                    DbValue::Integer(i as i64),
                    DbValue::Text(loc.to_string()),
                ],
            )?;
        }
    } else {
        let p1 = conn.placeholder(1);
        conn.execute(
            &format!("DELETE FROM {} WHERE parent_id = {p1}", table_name),
            &[DbValue::Text(parent_id.to_string())],
        )?;
        let (p1, p2, p3, p4) = (
            conn.placeholder(1),
            conn.placeholder(2),
            conn.placeholder(3),
            conn.placeholder(4),
        );
        let sql = format!(
            "INSERT INTO {} (parent_id, related_id, related_collection, _order) VALUES ({p1}, {p2}, {p3}, {p4})",
            table_name
        );
        for (i, (rel_col, rel_id)) in items.iter().enumerate() {
            conn.execute(
                &sql,
                &[
                    DbValue::Text(parent_id.to_string()),
                    DbValue::Text(rel_id.clone()),
                    DbValue::Text(rel_col.clone()),
                    DbValue::Integer(i as i64),
                ],
            )?;
        }
    }
    Ok(())
}

/// Find related items for a polymorphic has-many relationship junction table.
/// Returns `(related_collection, related_id)` pairs ordered by _order.
pub fn find_polymorphic_related(
    conn: &dyn DbConnection,
    collection: &str,
    field: &str,
    parent_id: &str,
    locale: Option<&str>,
) -> Result<Vec<(String, String)>> {
    let table_name = format!("{}_{}", collection, field);

    let (sql, params) = if let Some(loc) = locale {
        let (p1, p2) = (conn.placeholder(1), conn.placeholder(2));
        (
            format!(
                "SELECT related_collection, related_id FROM {} WHERE parent_id = {p1} AND _locale = {p2} ORDER BY _order",
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
                "SELECT related_collection, related_id FROM {} WHERE parent_id = {p1} ORDER BY _order",
                table_name
            ),
            vec![DbValue::Text(parent_id.to_string())],
        )
    };

    let rows = conn.query_all(&sql, &params)?;
    let items = rows
        .into_iter()
        .filter_map(|row| {
            let col = if let Some(DbValue::Text(s)) = row.get_value(0) {
                s.clone()
            } else {
                return None;
            };
            let id = if let Some(DbValue::Text(s)) = row.get_value(1) {
                s.clone()
            } else {
                return None;
            };
            Some((col, id))
        })
        .collect();
    Ok(items)
}

#[cfg(test)]
mod tests {
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

    fn setup_junction_db() -> (TempDir, BoxedConnection) {
        setup_conn(
            "CREATE TABLE posts_tags (
                parent_id TEXT,
                related_id TEXT,
                _order INTEGER
            );",
        )
    }

    fn setup_polymorphic_db() -> (TempDir, BoxedConnection) {
        setup_conn(
            "CREATE TABLE posts_refs (
                parent_id TEXT,
                related_id TEXT,
                related_collection TEXT NOT NULL DEFAULT '',
                _order INTEGER,
                PRIMARY KEY (parent_id, related_id, related_collection)
            );",
        )
    }

    // ── set_related_ids + find_related_ids ───────────────────────────────────

    #[test]
    fn set_and_find_related_ids() {
        let (_dir, conn) = setup_junction_db();
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
        let (_dir, conn) = setup_junction_db();
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
        let (_dir, conn) = setup_junction_db();
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
        let (_dir, conn) = setup_conn(
            "CREATE TABLE posts_tags (
                parent_id TEXT,
                related_id TEXT,
                _order INTEGER,
                _locale TEXT
            );",
        );

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
        let (_dir, conn) = setup_polymorphic_db();
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
        let (_dir, conn) = setup_polymorphic_db();
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
        let (_dir, conn) = setup_conn(
            "CREATE TABLE posts_refs (
                parent_id TEXT,
                related_id TEXT,
                related_collection TEXT NOT NULL DEFAULT '',
                _order INTEGER,
                _locale TEXT
            );",
        );

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
