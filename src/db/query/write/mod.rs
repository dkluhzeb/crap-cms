//! Write operations: create, update, delete.

mod create;
mod update;

pub use create::create;
pub(in crate::db::query) use update::{UpdateCollector, collect_update_params};
pub use update::{update, update_partial};

use anyhow::{Context as _, Result};

use crate::db::{DbConnection, DbValue};

/// Delete a document by ID. Returns `true` if a row was deleted, `false` if not found.
pub fn delete(conn: &dyn DbConnection, slug: &str, id: &str) -> Result<bool> {
    let sql = format!(
        "DELETE FROM \"{}\" WHERE id = {}",
        slug,
        conn.placeholder(1)
    );
    let affected = conn
        .execute(&sql, &[DbValue::Text(id.to_string())])
        .with_context(|| format!("Failed to delete document {} from '{}'", id, slug))?;
    Ok(affected > 0)
}

/// Soft-delete a document by setting `_deleted_at` to the current timestamp.
/// Returns `true` if a row was updated, `false` if not found or already deleted.
pub fn soft_delete(conn: &dyn DbConnection, slug: &str, id: &str) -> Result<bool> {
    let sql = format!(
        "UPDATE \"{}\" SET _deleted_at = {} WHERE id = {} AND _deleted_at IS NULL",
        slug,
        conn.now_expr(),
        conn.placeholder(1)
    );
    let affected = conn
        .execute(&sql, &[DbValue::Text(id.to_string())])
        .with_context(|| format!("Failed to soft-delete {} from '{}'", id, slug))?;
    Ok(affected > 0)
}

/// Restore a soft-deleted document by clearing `_deleted_at`.
/// Returns `true` if a row was restored, `false` if not found or not deleted.
///
/// If restoring would violate a unique constraint (because another active document
/// now has the same value), returns a descriptive error instead of a raw DB error.
pub fn restore(conn: &dyn DbConnection, slug: &str, id: &str) -> Result<bool> {
    let sql = format!(
        "UPDATE \"{}\" SET _deleted_at = NULL WHERE id = {} AND _deleted_at IS NOT NULL",
        slug,
        conn.placeholder(1)
    );

    match conn.execute(&sql, &[DbValue::Text(id.to_string())]) {
        Ok(affected) => Ok(affected > 0),
        Err(e) => {
            let msg = format!("{e:#}");

            if msg.contains("UNIQUE constraint failed") || msg.contains("unique constraint") {
                anyhow::bail!(
                    "Cannot restore document '{}' in '{}': a unique field value \
                     is already in use by another active document",
                    id,
                    slug
                );
            }

            Err(e).with_context(|| format!("Failed to restore {} in '{}'", id, slug))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::create::create;
    use super::*;
    use crate::config::CrapConfig;
    use crate::core::collection::*;
    use crate::core::field::*;
    use crate::db::{BoxedConnection, pool};
    use std::collections::HashMap;
    use tempfile::TempDir;

    fn test_def() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("status", FieldType::Text).build(),
        ];
        def
    }

    fn setup_db() -> (TempDir, BoxedConnection) {
        let dir = TempDir::new().unwrap();
        let config = CrapConfig::default();
        let p = pool::create_pool(dir.path(), &config).unwrap();
        let conn = p.get().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                title TEXT,
                status TEXT,
                created_at TEXT,
                updated_at TEXT
            )",
        )
        .unwrap();
        (dir, conn)
    }

    #[test]
    fn delete_basic() {
        let (_dir, conn) = setup_db();
        let def = test_def();
        let data = HashMap::new();

        let doc = create(&conn, "posts", &def, &data, None).unwrap();
        let id = doc.id.clone();

        let deleted = delete(&conn, "posts", &id).unwrap();
        assert!(deleted, "delete should return true when a row is deleted");

        let row = conn
            .query_one(
                "SELECT id FROM posts WHERE id = ?1",
                &[DbValue::Text(id.to_string())],
            )
            .unwrap();
        assert!(row.is_none(), "Document should be gone after delete");
    }

    #[test]
    fn delete_nonexistent_returns_false() {
        let (_dir, conn) = setup_db();
        let deleted = delete(&conn, "posts", "does-not-exist").unwrap();
        assert!(
            !deleted,
            "delete should return false for non-existent document"
        );
    }

    fn setup_soft_delete_db() -> (TempDir, BoxedConnection) {
        let dir = TempDir::new().unwrap();
        let config = CrapConfig::default();
        let p = pool::create_pool(dir.path(), &config).unwrap();
        let conn = p.get().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                title TEXT,
                status TEXT,
                _deleted_at TEXT,
                created_at TEXT,
                updated_at TEXT
            )",
        )
        .unwrap();
        (dir, conn)
    }

    #[test]
    fn soft_delete_sets_deleted_at() {
        let (_dir, conn) = setup_soft_delete_db();
        let def = test_def();
        let data = HashMap::new();
        let doc = create(&conn, "posts", &def, &data, None).unwrap();
        let id = doc.id.clone();

        let result = soft_delete(&conn, "posts", &id).unwrap();
        assert!(result, "soft_delete should return true");

        let row = conn
            .query_one(
                "SELECT _deleted_at FROM posts WHERE id = ?1",
                &[DbValue::Text(id.to_string())],
            )
            .unwrap();
        assert!(row.is_some(), "Document should still exist");

        let deleted_at = row.unwrap().get_opt_string("_deleted_at").unwrap();
        assert!(
            deleted_at.is_some(),
            "_deleted_at should be set after soft delete"
        );
    }

    #[test]
    fn soft_delete_returns_false_for_already_deleted() {
        let (_dir, conn) = setup_soft_delete_db();
        let def = test_def();
        let data = HashMap::new();
        let doc = create(&conn, "posts", &def, &data, None).unwrap();
        let id = doc.id.clone();

        soft_delete(&conn, "posts", &id).unwrap();
        let result = soft_delete(&conn, "posts", &id).unwrap();
        assert!(
            !result,
            "soft_delete should return false for already-deleted doc"
        );
    }

    #[test]
    fn restore_clears_deleted_at() {
        let (_dir, conn) = setup_soft_delete_db();
        let def = test_def();
        let data = HashMap::new();
        let doc = create(&conn, "posts", &def, &data, None).unwrap();
        let id = doc.id.clone();

        soft_delete(&conn, "posts", &id).unwrap();
        let result = restore(&conn, "posts", &id).unwrap();
        assert!(result, "restore should return true");

        let row = conn
            .query_one(
                "SELECT _deleted_at FROM posts WHERE id = ?1",
                &[DbValue::Text(id.to_string())],
            )
            .unwrap();
        let deleted_at = row.unwrap().get_opt_string("_deleted_at").unwrap();
        assert!(
            deleted_at.is_none(),
            "_deleted_at should be NULL after restore"
        );
    }

    #[test]
    fn restore_returns_false_for_non_deleted_doc() {
        let (_dir, conn) = setup_soft_delete_db();
        let def = test_def();
        let data = HashMap::new();
        let doc = create(&conn, "posts", &def, &data, None).unwrap();

        let result = restore(&conn, "posts", &doc.id).unwrap();
        assert!(!result, "restore should return false for non-deleted doc");
    }

    #[test]
    fn soft_delete_nonexistent_returns_false() {
        let (_dir, conn) = setup_soft_delete_db();
        let result = soft_delete(&conn, "posts", "does-not-exist").unwrap();
        assert!(
            !result,
            "soft_delete should return false for non-existent document"
        );
    }

    #[test]
    fn restore_nonexistent_returns_false() {
        let (_dir, conn) = setup_soft_delete_db();
        let result = restore(&conn, "posts", "does-not-exist").unwrap();
        assert!(
            !result,
            "restore should return false for non-existent document"
        );
    }

    fn setup_soft_delete_unique_db() -> (TempDir, BoxedConnection) {
        let dir = TempDir::new().unwrap();
        let config = CrapConfig::default();
        let p = pool::create_pool(dir.path(), &config).unwrap();
        let conn = p.get().unwrap();

        // Simulate a soft-delete collection with a partial unique index
        // (no inline UNIQUE — the partial index enforces uniqueness for active rows).
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                title TEXT,
                slug TEXT,
                status TEXT,
                _deleted_at TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            CREATE UNIQUE INDEX idx_posts_slug_active_unique
                ON posts (slug) WHERE _deleted_at IS NULL;",
        )
        .unwrap();

        (dir, conn)
    }

    #[test]
    fn soft_delete_then_create_with_same_unique_value() {
        let (_dir, conn) = setup_soft_delete_unique_db();

        // Insert and soft-delete a document
        conn.execute(
            "INSERT INTO posts (id, slug, _deleted_at) VALUES ('a', 'hello', '2025-01-01')",
            &[],
        )
        .unwrap();

        // Create a new document with the same slug — should succeed
        let result = conn.execute("INSERT INTO posts (id, slug) VALUES ('b', 'hello')", &[]);
        assert!(
            result.is_ok(),
            "Should allow creating doc with same slug as soft-deleted doc"
        );
    }

    #[test]
    fn restore_conflict_returns_descriptive_error() {
        let (_dir, conn) = setup_soft_delete_unique_db();

        // Insert and soft-delete a document
        conn.execute(
            "INSERT INTO posts (id, slug, _deleted_at) VALUES ('a', 'hello', '2025-01-01')",
            &[],
        )
        .unwrap();

        // Create a new active document reusing the same slug
        conn.execute("INSERT INTO posts (id, slug) VALUES ('b', 'hello')", &[])
            .unwrap();

        // Try to restore the soft-deleted document — should fail with conflict
        let result = restore(&conn, "posts", "a");
        assert!(
            result.is_err(),
            "Restore should fail due to unique conflict"
        );

        let err_msg = format!("{:#}", result.unwrap_err());
        assert!(
            err_msg.contains("unique field value"),
            "Error should mention unique conflict: {err_msg}"
        );
    }

    #[test]
    fn restore_succeeds_when_no_conflict() {
        let (_dir, conn) = setup_soft_delete_unique_db();

        // Insert and soft-delete a document
        conn.execute(
            "INSERT INTO posts (id, slug, _deleted_at) VALUES ('a', 'hello', '2025-01-01')",
            &[],
        )
        .unwrap();

        // Restore with no conflicting active document
        let result = restore(&conn, "posts", "a").unwrap();
        assert!(result, "Restore should succeed when no conflict exists");
    }
}
