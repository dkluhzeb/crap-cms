//! Reading a document's `_ref_count`, optionally under its row lock.

use anyhow::Result;

use crate::db::{DbConnection, DbValue};

/// Read the `_ref_count` value for a document.
/// Returns `None` if the document does not exist, `Some(count)` otherwise
/// (defaulting to 0 when the column is NULL).
///
/// # Errors
///
/// Returns a backend error if the SELECT query fails.
pub fn get_ref_count(conn: &dyn DbConnection, collection: &str, id: &str) -> Result<Option<i64>> {
    get_ref_count_inner(conn, collection, id, false)
}

/// Read `_ref_count` with a row-level lock (`SELECT ... FOR UPDATE` on Postgres).
///
/// Used by the delete path to prevent a concurrent create from incrementing the
/// ref count between the check and the actual DELETE. On `SQLite`, `IMMEDIATE`
/// transactions already serialize writes, so no lock suffix is needed.
///
/// # Errors
///
/// Returns a backend error if the SELECT query fails.
pub fn get_ref_count_locked(
    conn: &dyn DbConnection,
    collection: &str,
    id: &str,
) -> Result<Option<i64>> {
    get_ref_count_inner(conn, collection, id, true)
}

/// Lock a trashed row for purging and return its reference count.
///
/// `None` when the row is gone, no longer trashed, or — with
/// `older_than_seconds` set — trashed less than that long ago. Every purge of
/// the trash picks its candidates before it holds any lock, and a restore can
/// commit in between. Re-checking the trash state under the same `FOR UPDATE`
/// that guards the reference count keeps a just-restored document from being
/// hard-deleted.
///
/// # Errors
///
/// Returns a backend error if the SELECT fails.
pub fn get_purgeable_ref_count_locked(
    conn: &dyn DbConnection,
    collection: &str,
    id: &str,
    older_than_seconds: Option<i64>,
) -> Result<Option<i64>> {
    let mut params = vec![DbValue::Text(id.to_string())];

    let age_sql = match older_than_seconds {
        Some(seconds) => {
            let (offset_sql, offset_param) = conn.date_offset_expr(seconds, 2);
            params.push(offset_param);

            format!(" AND _deleted_at < {offset_sql}")
        }
        None => String::new(),
    };

    let for_update = if conn.is_postgres() {
        " FOR UPDATE"
    } else {
        ""
    };

    let sql = format!(
        "SELECT _ref_count FROM \"{collection}\" WHERE id = {} \
         AND _deleted_at IS NOT NULL{age_sql}{for_update}",
        conn.placeholder(1)
    );
    let row = conn.query_one(&sql, &params)?;

    Ok(row.map(|r| match r.get_value(0) {
        Some(DbValue::Integer(n)) => *n,
        _ => 0,
    }))
}

fn get_ref_count_inner(
    conn: &dyn DbConnection,
    collection: &str,
    id: &str,
    lock: bool,
) -> Result<Option<i64>> {
    let p1 = conn.placeholder(1);
    let for_update = if lock && conn.is_postgres() {
        " FOR UPDATE"
    } else {
        ""
    };
    let sql = format!("SELECT _ref_count FROM \"{collection}\" WHERE id = {p1}{for_update}");
    let row = conn.query_one(&sql, &[DbValue::Text(id.to_string())])?;

    Ok(row.map(|r| {
        r.get_value(0)
            .and_then(|v| match v {
                DbValue::Integer(n) => Some(*n),
                _ => None,
            })
            .unwrap_or(0)
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{core::CollectionDefinition, db::query::ref_count::test_helpers::*};

    // ── get_ref_count ────────────────────────────────────────────────────

    #[test]
    fn ref_count_defaults_to_zero() {
        let media = CollectionDefinition::new("media");
        let (_tmp, pool, _) = setup_db(&[media], &no_locale());
        let conn = pool.get().unwrap();

        insert_doc(&conn, "media", "m1");
        assert_eq!(get_ref_count_val(&conn, "media", "m1"), 0);
    }

    /// Regression: `get_ref_count` must return None for missing documents
    /// instead of 0, so callers can distinguish "not found" from "zero refs".
    #[test]
    fn ref_count_returns_none_for_missing_document() {
        let media = CollectionDefinition::new("media");
        let (_tmp, pool, _) = setup_db(&[media], &no_locale());
        let conn = pool.get().unwrap();

        let result = get_ref_count(&conn, "media", "nonexistent").unwrap();
        assert_eq!(result, None, "Missing document should return None");
    }

    // ── get_purgeable_ref_count_locked ───────────────────────────────────

    /// Only a row still trashed past retention is purgeable; a restored (live)
    /// or recently trashed row is skipped.
    #[test]
    fn purgeable_ref_count_requires_a_row_trashed_past_retention() {
        let mut posts = CollectionDefinition::new("posts");
        posts.soft_delete = true;
        let (_tmp, pool, _) = setup_db(&[posts], &no_locale());
        let conn = pool.get().unwrap();

        for id in ["live", "old", "recent"] {
            insert_doc(&conn, "posts", id);
        }
        conn.execute(
            "UPDATE posts SET _deleted_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-30 days') \
             WHERE id = 'old'",
            &[],
        )
        .unwrap();
        conn.execute(
            "UPDATE posts SET _deleted_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = 'recent'",
            &[],
        )
        .unwrap();

        let week = Some(7 * 86_400);
        assert_eq!(
            get_purgeable_ref_count_locked(&conn, "posts", "old", week).unwrap(),
            Some(0)
        );
        assert_eq!(
            get_purgeable_ref_count_locked(&conn, "posts", "recent", week).unwrap(),
            None
        );
        assert_eq!(
            get_purgeable_ref_count_locked(&conn, "posts", "live", week).unwrap(),
            None
        );
        assert_eq!(
            get_purgeable_ref_count_locked(&conn, "posts", "gone", week).unwrap(),
            None
        );

        // Without an age threshold any trashed row qualifies — a live one
        // never does.
        assert_eq!(
            get_purgeable_ref_count_locked(&conn, "posts", "recent", None).unwrap(),
            Some(0)
        );
        assert_eq!(
            get_purgeable_ref_count_locked(&conn, "posts", "live", None).unwrap(),
            None
        );
    }
}
