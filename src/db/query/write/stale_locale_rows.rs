//! The delete behind `db cleanup --confirm`: junction rows left behind by a
//! locale the project no longer configures. Its count twin lives with the
//! reads; both build their `WHERE` through the one shared clause.

use anyhow::Result;

use crate::db::{
    DbConnection,
    query::helpers::{outside_locales_clause, quote_ident},
};

/// Delete the rows of `table` whose `_locale` is outside `locales`, returning
/// how many were removed. A no-op for an empty `locales` list, matching
/// `count_rows_outside_locales`: no configured locales means localization is
/// off, not that every stored row should go.
///
/// The rows can hold references (a has-many relationship's junction rows, or
/// relationships inside array/blocks rows), so the caller owns the transaction
/// and recomputes the reference counts in it.
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
    use tempfile::TempDir;

    use super::*;
    use crate::{
        config::CrapConfig,
        db::{BoxedConnection, pool, query::count_rows_outside_locales},
    };

    fn seeded() -> (TempDir, BoxedConnection) {
        let dir = TempDir::new().unwrap();
        let p = pool::create_pool(dir.path(), &CrapConfig::default()).unwrap();
        let conn = p.get().unwrap();

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
