//! Write operations: create, update, delete.

mod create;
mod update;

pub use create::create;
pub use update::update;

use anyhow::{Context as _, Result};

/// Delete a document by ID.
pub fn delete(conn: &rusqlite::Connection, slug: &str, id: &str) -> Result<()> {
    let sql = format!("DELETE FROM {} WHERE id = ?1", slug);
    conn.execute(&sql, [id])
        .with_context(|| format!("Failed to delete document {} from '{}'", id, slug))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::read::find_by_id_raw;
    use super::create::create;
    use super::*;
    use crate::core::collection::*;
    use crate::core::field::*;
    use rusqlite::Connection;
    use std::collections::HashMap;

    fn test_def() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("status", FieldType::Text).build(),
        ];
        def
    }

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
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
        conn
    }

    #[test]
    fn delete_basic() {
        let conn = setup_db();
        let def = test_def();
        let data = HashMap::new();

        let doc = create(&conn, "posts", &def, &data, None).unwrap();
        let id = doc.id.clone();

        delete(&conn, "posts", &id).unwrap();

        let found = find_by_id_raw(&conn, "posts", &def, &id, None).unwrap();
        assert!(found.is_none(), "Document should be gone after delete");
    }

    #[test]
    fn delete_nonexistent() {
        let conn = setup_db();
        // Deleting a non-existent ID should not error (0 rows affected)
        let result = delete(&conn, "posts", "does-not-exist");
        assert!(result.is_ok(), "Deleting non-existent ID should not error");
    }
}
