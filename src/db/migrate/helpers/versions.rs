//! Versions table creation for document version history.

use anyhow::{Context as _, Result};
use tracing::info;

use crate::db::DbConnection;

use super::introspection::table_exists;

/// Create or verify the `_versions_{slug}` table for document version history.
pub(in crate::db::migrate) fn sync_versions_table(
    conn: &dyn DbConnection,
    slug: &str,
) -> Result<()> {
    let table_name = format!("_versions_{}", slug);

    if table_exists(conn, &table_name)? {
        return Ok(());
    }

    let sql = format!(
        "CREATE TABLE {} (\
            id TEXT PRIMARY KEY, \
            _parent TEXT NOT NULL REFERENCES {}(id) ON DELETE CASCADE, \
            _version INTEGER NOT NULL, \
            _status TEXT NOT NULL, \
            _latest INTEGER NOT NULL DEFAULT 0, \
            snapshot TEXT NOT NULL, \
            created_at {}, \
            updated_at {}\
        )",
        table_name,
        slug,
        conn.timestamp_column_default(),
        conn.timestamp_column_default()
    );

    info!("Creating versions table: {}", table_name);
    conn.execute_ddl(&sql, &[])
        .with_context(|| format!("Failed to create versions table {}", table_name))?;

    conn.execute_ddl(
        &format!(
            "CREATE INDEX IF NOT EXISTS idx__ver_{slug}_parent_latest ON {table} (_parent, _latest)",
            slug = slug,
            table = table_name
        ),
        &[],
    )?;

    conn.execute_ddl(
        &format!(
            "CREATE INDEX IF NOT EXISTS idx__ver_{slug}_parent_version ON {table} (_parent, _version DESC)",
            slug = slug, table = table_name
        ),
        &[],
    )?;

    conn.execute_ddl(
        &format!(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx__ver_{slug}_parent_version_unique ON {table} (_parent, _version)",
            slug = slug, table = table_name
        ),
        &[],
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::db::DbValue;
    use crate::db::migrate::collection::test_helpers::*;

    #[test]
    fn unique_parent_version_constraint() {
        let text = |s: &str| DbValue::Text(s.to_string());
        let int = |n: i64| DbValue::Integer(n);

        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        conn.execute("CREATE TABLE posts (id TEXT PRIMARY KEY)", &[])
            .unwrap();
        conn.execute("INSERT INTO posts (id) VALUES (?1)", &[text("p1")])
            .unwrap();
        sync_versions_table(&conn, "posts").unwrap();

        conn.execute(
            "INSERT INTO _versions_posts (id, _parent, _version, _status, snapshot) VALUES (?1, ?2, ?3, ?4, ?5)",
            &[text("v1"), text("p1"), int(1), text("published"), text("{}")],
        ).unwrap();

        // Duplicate (parent, version) should fail
        let err = conn.execute(
            "INSERT INTO _versions_posts (id, _parent, _version, _status, snapshot) VALUES (?1, ?2, ?3, ?4, ?5)",
            &[text("v2"), text("p1"), int(1), text("published"), text("{}")],
        );
        assert!(err.is_err());

        // Different version same parent should succeed
        conn.execute(
            "INSERT INTO _versions_posts (id, _parent, _version, _status, snapshot) VALUES (?1, ?2, ?3, ?4, ?5)",
            &[text("v3"), text("p1"), int(2), text("published"), text("{}")],
        ).unwrap();
    }

    #[test]
    fn indexes_use_ver_prefix() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        conn.execute("CREATE TABLE posts (id TEXT PRIMARY KEY)", &[])
            .unwrap();
        sync_versions_table(&conn, "posts").unwrap();

        let indexes: HashSet<String> = conn
            .query_all(
                "SELECT name FROM sqlite_master WHERE type='index' AND tbl_name=?1",
                &[DbValue::Text("_versions_posts".to_string())],
            )
            .unwrap()
            .into_iter()
            .filter_map(|r| r.get_string("name").ok())
            .collect();

        for idx_name in &indexes {
            assert!(!idx_name.starts_with("idx_posts_parent_"));
        }

        assert!(indexes.contains("idx__ver_posts_parent_latest"));
        assert!(indexes.contains("idx__ver_posts_parent_version"));
        assert!(indexes.contains("idx__ver_posts_parent_version_unique"));
    }
}
