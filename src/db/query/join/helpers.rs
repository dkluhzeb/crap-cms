//! Shared helpers for join table operations.

use anyhow::{Context as _, Result};

use crate::db::{DbConnection, DbValue, query::helpers::placeholder_list};

/// Build the SELECT for a junction/join table's rows for one parent — the
/// locale-optional `WHERE parent_id [AND _locale] ORDER BY _order` read that
/// mirrors [`delete_junction_rows`]. Returns the SQL (with `select_cols`
/// projected) and its bound params, so the array/blocks/relationship readers
/// can't drift on the WHERE branch, the `ORDER BY _order`, or the params vector.
pub(super) fn select_junction_rows(
    conn: &dyn DbConnection,
    table_name: &str,
    select_cols: &str,
    parent_id: &str,
    locale: Option<&str>,
) -> (String, Vec<DbValue>) {
    if let Some(loc) = locale {
        let (p1, p2) = (conn.placeholder(1), conn.placeholder(2));
        (
            format!(
                "SELECT {select_cols} FROM \"{table_name}\" \
                 WHERE parent_id = {p1} AND _locale = {p2} ORDER BY _order"
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
                "SELECT {select_cols} FROM \"{table_name}\" WHERE parent_id = {p1} ORDER BY _order"
            ),
            vec![DbValue::Text(parent_id.to_string())],
        )
    }
}

/// Batched twin of [`select_junction_rows`]: read junction rows for MANY
/// parents in one query — `WHERE parent_id IN (…) [AND _locale] ORDER BY
/// parent_id, _order`. Callers differ only in `select_cols`, so the WHERE
/// branch, the `IN (…)` placeholder numbering, and the ORDER BY live here once.
/// Caller must ensure `parent_ids` is non-empty (`IN ()` is invalid SQL).
pub(super) fn select_junction_rows_batch(
    conn: &dyn DbConnection,
    table_name: &str,
    select_cols: &str,
    parent_ids: &[&str],
    locale: Option<&str>,
) -> (String, Vec<DbValue>) {
    let in_placeholders = placeholder_list(conn, parent_ids.len());
    let mut params: Vec<DbValue> = parent_ids
        .iter()
        .map(|id| DbValue::Text((*id).to_string()))
        .collect();

    if let Some(loc) = locale {
        // The locale placeholder sits just past the IN list, at N+1.
        let loc_ph = conn.placeholder(parent_ids.len() + 1);
        params.push(DbValue::Text(loc.to_string()));

        return (
            format!(
                "SELECT {select_cols} FROM \"{table_name}\" \
                 WHERE parent_id IN ({in_placeholders}) AND _locale = {loc_ph} \
                 ORDER BY parent_id, _order"
            ),
            params,
        );
    }

    (
        format!(
            "SELECT {select_cols} FROM \"{table_name}\" \
             WHERE parent_id IN ({in_placeholders}) \
             ORDER BY parent_id, _order"
        ),
        params,
    )
}

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

#[cfg(all(test, feature = "sqlite"))]
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
