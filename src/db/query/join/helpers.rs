//! Shared helpers for join table operations.

use std::collections::HashSet;

use anyhow::{Context as _, Result};
use nanoid::nanoid;

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

/// The invariant destination of a diff-based junction write — identical across
/// every row of one `set_*_rows` call. Bundled so the per-row INSERT helpers
/// stay within the argument limit.
#[derive(Clone, Copy)]
pub(super) struct JunctionTarget<'a> {
    pub table_name: &'a str,
    pub parent_id: &'a str,
    pub locale: Option<&'a str>,
}

/// How a diff-based junction write treats an incoming row's `id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RowIds {
    /// Keep an id only when it names an existing row of this parent; every
    /// other row gets a server-minted id, so a client can neither choose a
    /// primary key nor address another parent's row.
    Existing,
    /// Keep every incoming id, minting only for a row without one: a raw
    /// restore of exported rows, which must come back under the ids they had.
    Incoming,
}

/// Plan one incoming row's identity as `(id, is_update)`. An id already
/// claimed earlier in the same write is never used twice.
pub(super) fn plan_row_id(
    incoming: Option<&str>,
    exists: impl Fn(&str) -> bool,
    claimed: &HashSet<String>,
    row_ids: RowIds,
) -> (String, bool) {
    match incoming.filter(|id| !id.is_empty() && !claimed.contains(*id)) {
        Some(id) if exists(id) => (id.to_string(), true),
        Some(id) if row_ids == RowIds::Incoming => (id.to_string(), false),
        _ => (nanoid!(), false),
    }
}

/// The set of existing junction-row ids for one parent[+locale]. The
/// diff-based array/blocks writers use it to tell an incoming row that *updates*
/// an existing row (its `id` is in this set) from one that *inserts* a new row.
pub(super) fn existing_junction_ids(
    conn: &dyn DbConnection,
    table_name: &str,
    parent_id: &str,
    locale: Option<&str>,
) -> Result<HashSet<String>> {
    let (sql, params) = select_junction_rows(conn, table_name, "id", parent_id, locale);

    let rows = conn
        .query_all(&sql, &params)
        .with_context(|| format!("Failed to read junction ids from {table_name}"))?;

    Ok(rows
        .iter()
        .filter_map(|r| match r.get_value(0) {
            Some(DbValue::Text(s)) => Some(s.clone()),
            _ => None,
        })
        .collect())
}

/// Delete the junction rows for a parent[+locale] whose id is NOT in `keep` —
/// the diff-based twin of [`delete_junction_rows`]. It removes only the rows the
/// incoming set dropped, leaving matched rows in place for a column-preserving
/// UPDATE. An empty `keep` deletes every row (identical to
/// [`delete_junction_rows`]).
pub(super) fn delete_junction_rows_except(
    conn: &dyn DbConnection,
    table_name: &str,
    parent_id: &str,
    locale: Option<&str>,
    keep: &HashSet<String>,
) -> Result<()> {
    if keep.is_empty() {
        return delete_junction_rows(conn, table_name, parent_id, locale);
    }

    let p_parent = conn.placeholder(1);
    let mut params: Vec<DbValue> = vec![DbValue::Text(parent_id.to_string())];

    let (locale_clause, in_start) = if let Some(loc) = locale {
        let p_loc = conn.placeholder(2);
        params.push(DbValue::Text(loc.to_string()));
        (format!(" AND _locale = {p_loc}"), 3)
    } else {
        (String::new(), 2)
    };

    let mut in_phs = Vec::with_capacity(keep.len());
    for (i, id) in keep.iter().enumerate() {
        in_phs.push(conn.placeholder(in_start + i));
        params.push(DbValue::Text(id.clone()));
    }
    let in_list = in_phs.join(", ");

    let sql = format!(
        "DELETE FROM \"{table_name}\" \
         WHERE parent_id = {p_parent}{locale_clause} AND id NOT IN ({in_list})"
    );

    conn.execute(&sql, &params)
        .with_context(|| format!("Failed to prune junction table {table_name}"))?;

    Ok(())
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use super::*;
    use crate::db::InMemoryConn;

    /// An existing row keeps its id either way; an unknown id is kept only for
    /// a restore, and never twice in one write.
    #[test]
    fn plan_row_id_keeps_incoming_ids_only_for_a_restore() {
        let none = HashSet::new();
        let exists = |id: &str| id == "stored";

        assert_eq!(
            plan_row_id(Some("stored"), exists, &none, RowIds::Existing),
            ("stored".to_string(), true)
        );

        let (minted, is_update) = plan_row_id(Some("exported"), exists, &none, RowIds::Existing);
        assert!(!is_update);
        assert_ne!(minted, "exported");

        assert_eq!(
            plan_row_id(Some("exported"), exists, &none, RowIds::Incoming),
            ("exported".to_string(), false)
        );

        let claimed: HashSet<String> = ["exported".to_string()].into();
        assert_ne!(
            plan_row_id(Some("exported"), exists, &claimed, RowIds::Incoming).0,
            "exported"
        );
    }

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
