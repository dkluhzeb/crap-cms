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
    let sql = format!("DELETE FROM {} WHERE id = {}", slug, conn.placeholder(1));
    let affected = conn
        .execute(&sql, &[DbValue::Text(id.to_string())])
        .with_context(|| format!("Failed to delete document {} from '{}'", id, slug))?;
    Ok(affected > 0)
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
}
