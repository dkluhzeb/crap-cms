//! The trash timestamp of a stored row, whichever definition reads it.

use anyhow::Result;
use serde_json::Value;

use crate::db::{DbConnection, DbValue};

/// The stored `_deleted_at` of row `id` in `slug`'s table: `Some` — the trash
/// timestamp, or `null` for a live row — when the table has a trash column,
/// `None` when it has none or the row does not exist.
///
/// Reads every column instead of naming `_deleted_at`, so it answers for a
/// collection read through its hard-delete variant (which selects no trash
/// column) as for one that never had soft delete, without failing on the
/// missing column.
///
/// # Errors
///
/// Returns a backend error if the SELECT fails.
pub fn stored_deleted_at(conn: &dyn DbConnection, slug: &str, id: &str) -> Result<Option<Value>> {
    let p1 = conn.placeholder(1);
    let sql = format!("SELECT * FROM \"{slug}\" WHERE id = {p1}");

    let Some(row) = conn.query_one(&sql, &[DbValue::Text(id.to_string())])? else {
        return Ok(None);
    };

    Ok(row.get_named("_deleted_at").map(DbValue::to_json))
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use serde_json::json;
    use tempfile::TempDir;

    use super::*;
    use crate::{
        config::CrapConfig,
        db::{BoxedConnection, pool::create_pool},
    };

    fn db() -> (TempDir, BoxedConnection) {
        let dir = TempDir::new().unwrap();
        let pool = create_pool(dir.path(), &CrapConfig::default()).unwrap();
        let conn = pool.get().unwrap();

        conn.execute_batch(
            "CREATE TABLE trashable (id TEXT PRIMARY KEY, _deleted_at TEXT);
             CREATE TABLE plain (id TEXT PRIMARY KEY);
             INSERT INTO trashable VALUES ('gone', '2026-01-01T00:00:00.000Z'), ('live', NULL);
             INSERT INTO plain VALUES ('p');",
        )
        .unwrap();

        (dir, conn)
    }

    #[test]
    fn reads_the_trash_timestamp_or_null() {
        let (_dir, conn) = db();

        assert_eq!(
            stored_deleted_at(&conn, "trashable", "gone").unwrap(),
            Some(json!("2026-01-01T00:00:00.000Z"))
        );
        assert_eq!(
            stored_deleted_at(&conn, "trashable", "live").unwrap(),
            Some(Value::Null)
        );
    }

    #[test]
    fn a_table_without_a_trash_column_or_a_missing_row_has_none() {
        let (_dir, conn) = db();

        assert_eq!(stored_deleted_at(&conn, "plain", "p").unwrap(), None);
        assert_eq!(
            stored_deleted_at(&conn, "trashable", "absent").unwrap(),
            None
        );
    }
}
