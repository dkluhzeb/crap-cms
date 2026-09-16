//! Reads and deletes backing `db cleanup`: junction rows left behind by a
//! locale the project no longer configures.
//!
//! A join table (array, blocks, has-many relationship) for a localized field
//! carries a `_locale` column. Removing a locale from `[locale] locales` leaves
//! its rows in place — invisible to every read, counted by nothing, and never
//! cleaned up by a migration. The cleanup command decides WHICH tables to look
//! at; these execute the lookups, keeping the SQL in the `db` module.

use anyhow::Result;

use crate::db::{DbConnection, DbValue, query::helpers::quote_ident};

/// `WHERE _locale NOT IN (…)` plus the bound locale values, for a table whose
/// rows belong to `locales`. One builder for the count and the delete so they
/// can never disagree about which rows are stale.
fn outside_locales_clause(conn: &dyn DbConnection, locales: &[String]) -> (String, Vec<DbValue>) {
    let placeholders = (1..=locales.len())
        .map(|i| conn.placeholder(i))
        .collect::<Vec<_>>()
        .join(", ");

    let params = locales
        .iter()
        .map(|l| DbValue::Text(l.clone()))
        .collect::<Vec<_>>();

    (format!("_locale NOT IN ({placeholders})"), params)
}

/// How many rows of `table` carry a `_locale` outside `locales`.
///
/// Returns `0` for an empty `locales` list rather than treating every row as
/// stale — "no configured locales" means localization is off, not that every
/// stored row should go.
///
/// # Errors
///
/// Returns a backend error if the query fails.
pub fn count_rows_outside_locales(
    conn: &dyn DbConnection,
    table: &str,
    locales: &[String],
) -> Result<i64> {
    if locales.is_empty() {
        return Ok(0);
    }

    let (clause, params) = outside_locales_clause(conn, locales);
    let sql = format!(
        "SELECT COUNT(*) as count FROM {} WHERE {clause}",
        quote_ident(table)
    );

    let row = conn.query_one(&sql, &params)?;

    // Read positionally and match the integer shape, as the unique-count query
    // does: a backend may name the aggregate column differently or widen it.
    let count = row
        .as_ref()
        .and_then(|r| r.get_value(0))
        .and_then(|v| match v {
            DbValue::Integer(i) => Some(*i),
            _ => None,
        })
        .unwrap_or(0);

    Ok(count)
}

/// Delete the rows of `table` whose `_locale` is outside `locales`, returning
/// how many were removed. A no-op for an empty `locales` list, matching
/// [`count_rows_outside_locales`].
///
/// # Errors
///
/// Returns a backend error if the delete fails.
pub fn delete_rows_outside_locales(
    conn: &dyn DbConnection,
    table: &str,
    locales: &[String],
) -> Result<usize> {
    if locales.is_empty() {
        return Ok(0);
    }

    let (clause, params) = outside_locales_clause(conn, locales);
    let sql = format!("DELETE FROM {} WHERE {clause}", quote_ident(table));

    conn.execute(&sql, &params)
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use super::*;
    use crate::{
        config::CrapConfig,
        db::{BoxedConnection, pool},
    };
    use tempfile::TempDir;

    fn make_conn() -> (TempDir, BoxedConnection) {
        let dir = TempDir::new().unwrap();
        let cfg = CrapConfig::default();
        let p = pool::create_pool(dir.path(), &cfg).unwrap();
        let conn = p.get().unwrap();

        (dir, conn)
    }

    fn seeded() -> (TempDir, BoxedConnection) {
        let (dir, conn) = make_conn();
        conn.execute_batch(
            "CREATE TABLE posts_items (id TEXT PRIMARY KEY, parent_id TEXT, _locale TEXT);
             INSERT INTO posts_items VALUES ('a', 'p1', 'en');
             INSERT INTO posts_items VALUES ('b', 'p1', 'de');
             INSERT INTO posts_items VALUES ('c', 'p1', 'fr');
             INSERT INTO posts_items VALUES ('d', 'p2', 'fr');",
        )
        .unwrap();

        (dir, conn)
    }

    fn configured() -> Vec<String> {
        vec!["en".to_string(), "de".to_string()]
    }

    #[test]
    fn counts_only_the_rows_of_unconfigured_locales() {
        let (_dir, conn) = seeded();

        assert_eq!(
            count_rows_outside_locales(&conn, "posts_items", &configured()).unwrap(),
            2,
            "both `fr` rows are stale, the `en`/`de` ones are not"
        );
    }

    #[test]
    fn deletes_only_the_rows_of_unconfigured_locales() {
        let (_dir, conn) = seeded();

        assert_eq!(
            delete_rows_outside_locales(&conn, "posts_items", &configured()).unwrap(),
            2
        );
        assert_eq!(
            count_rows_outside_locales(&conn, "posts_items", &configured()).unwrap(),
            0
        );

        let remaining = conn
            .query_all("SELECT id FROM posts_items ORDER BY id", &[])
            .unwrap();
        assert_eq!(remaining.len(), 2, "the configured locales' rows stay");
    }

    /// No configured locales means localization is off — every row is kept,
    /// never treated as stale.
    #[test]
    fn an_empty_locale_list_is_a_no_op() {
        let (_dir, conn) = seeded();

        assert_eq!(
            count_rows_outside_locales(&conn, "posts_items", &[]).unwrap(),
            0
        );
        assert_eq!(
            delete_rows_outside_locales(&conn, "posts_items", &[]).unwrap(),
            0
        );
        assert_eq!(
            conn.query_all("SELECT id FROM posts_items", &[])
                .unwrap()
                .len(),
            4
        );
    }
}
