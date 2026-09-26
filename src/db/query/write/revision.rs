//! The document revision counter behind optimistic locking.
//!
//! Every collection and global row carries `_revision`, a counter every write
//! that changes the document moves one forward — in the same transaction, so a
//! rolled-back write leaves it where it was. A caller that read the document at
//! revision `n` and sends `n` back with its write is refused when anyone wrote
//! in between, instead of silently overwriting that write.

use anyhow::{Context as _, Result};

pub use crate::core::REVISION_COLUMN;

use crate::db::{DbConnection, DbValue, query::helpers::quote_ident};

/// What [`advance_revision`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevisionAdvance {
    /// The revision moved one forward.
    Advanced,
    /// The row holds another revision — the one carried — than the caller
    /// expected; nothing changed.
    Stale(i64),
    /// No row has that id.
    Missing,
}

/// Move the revision of row `id` in `table` one forward — when `expected` is
/// set, only if the row still holds exactly that revision.
///
/// The comparison and the bump are one `UPDATE`, so no concurrent writer can
/// slip between them: of two writers that both expect revision `n`, the second
/// one's statement re-reads the row after the first committed (Postgres) or
/// waits for its transaction (`SQLite`), finds `n + 1`, and matches nothing.
///
/// # Errors
///
/// Returns a backend error if the UPDATE or the existence check fails.
pub fn advance_revision(
    conn: &dyn DbConnection,
    table: &str,
    id: &str,
    expected: Option<i64>,
) -> Result<RevisionAdvance> {
    let mut params = vec![DbValue::Text(id.to_string())];

    let guard = match expected {
        Some(revision) => {
            params.push(DbValue::Integer(revision));
            format!(" AND {REVISION_COLUMN} = {}", conn.placeholder(2))
        }
        None => String::new(),
    };

    let sql = format!(
        "UPDATE {} SET {REVISION_COLUMN} = {REVISION_COLUMN} + 1 WHERE id = {}{guard}",
        quote_ident(table),
        conn.placeholder(1)
    );

    let affected = conn
        .execute(&sql, &params)
        .with_context(|| format!("Failed to advance the revision of {table}.{id}"))?;

    if affected > 0 {
        return Ok(RevisionAdvance::Advanced);
    }

    Ok(read_revision(conn, table, id)?.map_or(RevisionAdvance::Missing, RevisionAdvance::Stale))
}

/// The current revision of row `id` in `table`, or `None` when no row has
/// that id.
///
/// # Errors
///
/// Returns a backend error if the SELECT fails.
pub fn read_revision(conn: &dyn DbConnection, table: &str, id: &str) -> Result<Option<i64>> {
    let sql = format!(
        "SELECT {REVISION_COLUMN} FROM {} WHERE id = {}",
        quote_ident(table),
        conn.placeholder(1)
    );

    let row = conn
        .query_one(&sql, &[DbValue::Text(id.to_string())])
        .with_context(|| format!("Failed to read the revision of {table}.{id}"))?;

    row.map(|row| row.get_i64(REVISION_COLUMN)).transpose()
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use rusqlite::Connection;

    use super::*;

    /// One `posts` row at revision 3.
    fn posts() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (id TEXT PRIMARY KEY, title TEXT,
                 _revision INTEGER NOT NULL DEFAULT 0);
             INSERT INTO posts (id, title, _revision) VALUES ('p1', 'a', 3);",
        )
        .unwrap();

        conn
    }

    #[test]
    fn an_unconditional_advance_moves_the_revision_one_forward() {
        let conn = posts();

        assert_eq!(
            advance_revision(&conn, "posts", "p1", None).unwrap(),
            RevisionAdvance::Advanced
        );
        assert_eq!(read_revision(&conn, "posts", "p1").unwrap(), Some(4));
    }

    #[test]
    fn a_matching_expected_revision_advances() {
        let conn = posts();

        assert_eq!(
            advance_revision(&conn, "posts", "p1", Some(3)).unwrap(),
            RevisionAdvance::Advanced
        );
        assert_eq!(read_revision(&conn, "posts", "p1").unwrap(), Some(4));
    }

    /// A writer that read an older revision is refused and changes nothing:
    /// the second of two editors who both loaded revision 3 must not land.
    #[test]
    fn a_stale_expected_revision_is_refused_without_a_change() {
        let conn = posts();

        assert_eq!(
            advance_revision(&conn, "posts", "p1", Some(3)).unwrap(),
            RevisionAdvance::Advanced
        );
        assert_eq!(
            advance_revision(&conn, "posts", "p1", Some(3)).unwrap(),
            RevisionAdvance::Stale(4)
        );
        assert_eq!(read_revision(&conn, "posts", "p1").unwrap(), Some(4));
    }

    #[test]
    fn a_missing_row_is_reported_as_missing() {
        let conn = posts();

        for expected in [None, Some(0)] {
            assert_eq!(
                advance_revision(&conn, "posts", "ghost", expected).unwrap(),
                RevisionAdvance::Missing
            );
        }
        assert_eq!(read_revision(&conn, "posts", "ghost").unwrap(), None);
    }
}
