//! Shared helpers for join table operations.

use anyhow::{Context as _, Result};

use crate::db::{DbConnection, DbValue};

/// Delete rows from a junction/join table for a given parent, optionally filtered by locale.
pub(super) fn delete_junction_rows(
    conn: &dyn DbConnection,
    table_name: &str,
    parent_id: &str,
    locale: Option<&str>,
) -> Result<()> {
    if let Some(loc) = locale {
        let (p1, p2) = (conn.placeholder(1), conn.placeholder(2));

        conn.execute(
            &format!("DELETE FROM \"{table_name}\" WHERE parent_id = {p1} AND _locale = {p2}"),
            &[
                DbValue::Text(parent_id.to_string()),
                DbValue::Text(loc.to_string()),
            ],
        )
        .with_context(|| format!("Failed to clear join table {table_name}"))?;
    } else {
        let p1 = conn.placeholder(1);

        conn.execute(
            &format!("DELETE FROM \"{table_name}\" WHERE parent_id = {p1}"),
            &[DbValue::Text(parent_id.to_string())],
        )
        .with_context(|| format!("Failed to clear join table {table_name}"))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::InMemoryConn;

    fn setup() -> InMemoryConn {
        let conn = InMemoryConn::open();
        conn.setup(
            "CREATE TABLE posts_tags (parent_id TEXT, related_id TEXT, _locale TEXT);
             INSERT INTO posts_tags VALUES
               ('p1', 't1', 'en'),
               ('p1', 't2', 'de'),
               ('p2', 't3', 'en');",
        );
        conn
    }

    fn count(conn: &InMemoryConn, where_clause: &str) -> i64 {
        conn.query_one(
            &format!("SELECT COUNT(*) FROM posts_tags WHERE {where_clause}"),
            &[],
        )
        .unwrap()
        .unwrap()
        .i64_at(0)
        .unwrap()
    }

    #[test]
    fn delete_without_locale_removes_all_rows_for_the_parent_only() {
        let conn = setup();
        delete_junction_rows(&conn, "posts_tags", "p1", None).unwrap();
        assert_eq!(count(&conn, "parent_id = 'p1'"), 0, "all p1 rows removed");
        assert_eq!(count(&conn, "parent_id = 'p2'"), 1, "p2 left untouched");
    }

    #[test]
    fn delete_with_locale_removes_only_that_locale_for_the_parent() {
        let conn = setup();
        delete_junction_rows(&conn, "posts_tags", "p1", Some("en")).unwrap();
        assert_eq!(
            count(&conn, "parent_id = 'p1' AND _locale = 'en'"),
            0,
            "p1/en removed"
        );
        assert_eq!(
            count(&conn, "parent_id = 'p1' AND _locale = 'de'"),
            1,
            "p1/de kept — other locales survive"
        );
        assert_eq!(count(&conn, "parent_id = 'p2'"), 1, "p2 left untouched");
    }
}
