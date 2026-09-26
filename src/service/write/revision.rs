//! The revision step every document write takes under its row lock.

use crate::{
    db::{
        DbConnection,
        query::{self, RevisionAdvance},
    },
    service::{RevisionConflict, ServiceError},
};

/// Move the revision of row `id` in `table` one forward, inside the write's
/// transaction and under its row lock — refusing the write with
/// [`ServiceError::Conflict`] when the caller sent an `expected` revision the
/// row no longer holds.
///
/// Every write that changes a document takes this step exactly once — update,
/// draft save, publish, unpublish, restore, on collections and globals alike —
/// so a revision an editor loaded goes stale the moment anyone else's change
/// lands, whichever surface made it. Without an `expected` revision the write
/// is unconditional (last write wins); a row that does not exist is then left
/// for the write's own lookup to report, as it always did.
///
/// # Errors
///
/// Returns [`ServiceError::Conflict`] for a stale expected revision,
/// [`ServiceError::NotFound`] when an expected revision names a row that does
/// not exist, or a backend error.
pub(crate) fn claim_revision(
    conn: &dyn DbConnection,
    table: &str,
    id: &str,
    expected: Option<i64>,
) -> Result<(), ServiceError> {
    let advance = query::advance_revision(conn, table, id, expected)?;

    match (advance, expected) {
        (RevisionAdvance::Stale(current), Some(expected)) => Err(ServiceError::Conflict(
            RevisionConflict::new(expected, current),
        )),
        (RevisionAdvance::Missing, Some(_)) => Err(ServiceError::NotFound(format!(
            "Document '{id}' not found in '{table}'"
        ))),
        _ => Ok(()),
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use rusqlite::Connection;

    use super::*;

    /// One `posts` row at revision 2.
    fn posts() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (id TEXT PRIMARY KEY, _revision INTEGER NOT NULL DEFAULT 0);
             INSERT INTO posts (id, _revision) VALUES ('p1', 2);",
        )
        .unwrap();

        conn
    }

    #[test]
    fn a_write_without_an_expected_revision_always_advances() {
        let conn = posts();

        claim_revision(&conn, "posts", "p1", None).unwrap();
        claim_revision(&conn, "posts", "p1", None).unwrap();

        assert_eq!(query::read_revision(&conn, "posts", "p1").unwrap(), Some(4));
    }

    #[test]
    fn a_current_expected_revision_is_admitted() {
        let conn = posts();

        claim_revision(&conn, "posts", "p1", Some(2)).unwrap();

        assert_eq!(query::read_revision(&conn, "posts", "p1").unwrap(), Some(3));
    }

    /// The second of two writers that loaded the same revision is refused
    /// with a conflict naming the revision it sent and the one the row holds,
    /// and the row stays where the first writer left it.
    #[test]
    fn a_stale_expected_revision_is_a_conflict() {
        let conn = posts();

        claim_revision(&conn, "posts", "p1", Some(2)).unwrap();
        let err = claim_revision(&conn, "posts", "p1", Some(2)).unwrap_err();

        assert!(
            matches!(err, ServiceError::Conflict(c) if c == RevisionConflict::new(2, 3)),
            "{err:?}"
        );
        assert_eq!(query::read_revision(&conn, "posts", "p1").unwrap(), Some(3));
    }

    /// An expected revision for a row that does not exist is a not-found; an
    /// unconditional write leaves that to its own lookup.
    #[test]
    fn a_missing_row_is_not_found_only_under_a_precondition() {
        let conn = posts();

        assert!(matches!(
            claim_revision(&conn, "posts", "ghost", Some(0)),
            Err(ServiceError::NotFound(_))
        ));
        assert!(claim_revision(&conn, "posts", "ghost", None).is_ok());
    }
}
