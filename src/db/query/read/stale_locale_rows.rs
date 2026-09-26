//! Reads backing `db cleanup`: junction rows left behind by a locale the
//! project no longer configures.
//!
//! A join table (array, blocks, has-many relationship) for a localized field
//! carries a `_locale` column. Removing a locale from `[locale] locales` leaves
//! its rows in place — invisible to every read, counted by nothing, and never
//! cleaned up by a migration. The cleanup command decides WHICH tables to look
//! at; this executes the lookup, keeping the SQL in the `db` module. The delete
//! lives with the writes and builds its `WHERE` through the same clause.

use anyhow::Result;

use crate::db::{
    DbConnection, DbValue,
    query::helpers::{outside_locales_clause, quote_ident},
};

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

/// The distinct locales `table`'s rows are stored under, sorted.
///
/// # Errors
///
/// Returns a backend error if the query fails.
pub fn held_locales(conn: &dyn DbConnection, table: &str) -> Result<Vec<String>> {
    let sql = format!(
        "SELECT DISTINCT _locale FROM {} WHERE _locale IS NOT NULL ORDER BY _locale",
        quote_ident(table)
    );

    let rows = conn.query_all(&sql, &[])?;

    Ok(rows.iter().filter_map(|row| row.opt_text_at(0)).collect())
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
    fn lists_the_locales_rows_are_held_under() {
        let (_dir, conn) = seeded();

        assert_eq!(
            held_locales(&conn, "posts_items").unwrap(),
            vec!["de", "en", "fr"]
        );
    }

    /// No configured locales means localization is off — no row counts as
    /// stale.
    #[test]
    fn an_empty_locale_list_counts_nothing() {
        let (_dir, conn) = seeded();

        assert_eq!(
            count_rows_outside_locales(&conn, "posts_items", &[]).unwrap(),
            0
        );
    }
}
