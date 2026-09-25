//! Where a stored row sits across the content views, read on its own.

use anyhow::{Context as _, Result};

use crate::{
    core::EventViewPlacement,
    db::{DbConnection, DbValue},
};

/// The row `id` of `table` placed across the content views — its `_status`
/// and, when `trash` (the table keeps a `_deleted_at` column), whether it is
/// trashed — reading only those columns. `None` when there is no such row.
///
/// `table` must have a `_status` column (a collection or global with drafts).
///
/// # Errors
///
/// Returns a backend error if the SELECT fails.
pub fn find_view_placement(
    conn: &dyn DbConnection,
    table: &str,
    id: &str,
    trash: bool,
) -> Result<Option<EventViewPlacement>> {
    let columns = if trash {
        "_status, _deleted_at"
    } else {
        "_status"
    };

    let row = conn
        .query_one(
            &format!(
                "SELECT {columns} FROM \"{table}\" WHERE id = {}",
                conn.placeholder(1)
            ),
            &[DbValue::Text(id.to_string())],
        )
        .with_context(|| format!("Failed to read the placement of {table}.{id}"))?;

    let Some(row) = row else {
        return Ok(None);
    };

    // Compared as a value, not read as text: Postgres returns the column as
    // a timestamp.
    let trashed = trash
        && row
            .get_named("_deleted_at")
            .is_some_and(|value| *value != DbValue::Null);

    Ok(Some(EventViewPlacement {
        status: row.get_opt_string("_status")?,
        trashed,
    }))
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use super::*;
    use crate::db::InMemoryConn;

    fn posts(conn: &InMemoryConn) {
        conn.execute_ddl(
            "CREATE TABLE posts (id TEXT PRIMARY KEY, title TEXT, _status TEXT, _deleted_at TEXT)",
            &[],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO posts (id, _status, _deleted_at) VALUES \
             ('live', 'published', NULL), ('binned', 'draft', '2026-01-01T00:00:00Z')",
            &[],
        )
        .unwrap();
    }

    #[test]
    fn the_placement_names_status_and_trash() {
        let conn = InMemoryConn::open();
        posts(&conn);

        assert_eq!(
            find_view_placement(&conn, "posts", "live", true).unwrap(),
            Some(EventViewPlacement {
                status: Some("published".into()),
                trashed: false,
            })
        );
        assert_eq!(
            find_view_placement(&conn, "posts", "binned", true).unwrap(),
            Some(EventViewPlacement {
                status: Some("draft".into()),
                trashed: true,
            })
        );
    }

    /// A table without a trash column is read without it.
    #[test]
    fn without_trash_only_the_status_is_read() {
        let conn = InMemoryConn::open();
        conn.execute_ddl("CREATE TABLE g (id TEXT PRIMARY KEY, _status TEXT)", &[])
            .unwrap();
        conn.execute(
            "INSERT INTO g (id, _status) VALUES ('default', 'draft')",
            &[],
        )
        .unwrap();

        let placement = find_view_placement(&conn, "g", "default", false)
            .unwrap()
            .unwrap();

        assert_eq!(placement.status.as_deref(), Some("draft"));
        assert!(!placement.trashed);
        assert!(
            find_view_placement(&conn, "g", "missing", false)
                .unwrap()
                .is_none()
        );
    }
}
