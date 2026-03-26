//! Version CRUD operations and document status management.

use anyhow::{Context as _, Result};
use serde_json::Value;

use crate::{
    core::document::VersionSnapshot,
    db::{DbConnection, DbValue},
};

/// Create a new version entry. Clears previous `_latest` flag, inserts new version.
pub fn create_version(
    conn: &dyn DbConnection,
    slug: &str,
    parent_id: &str,
    status: &str,
    snapshot: &Value,
) -> Result<VersionSnapshot> {
    let table = format!("_versions_{}", slug);
    let id = nanoid::nanoid!();

    // Get the next version number
    let p1 = conn.placeholder(1);
    let next_version: i64 = conn
        .query_one(
            &format!(
                "SELECT COALESCE(MAX(_version), 0) + 1 AS next_ver FROM {} WHERE _parent = {p1}",
                table
            ),
            &[DbValue::Text(parent_id.to_string())],
        )?
        .map(|row| row.get_i64("next_ver"))
        .transpose()?
        .unwrap_or(1);

    // Clear previous _latest flag
    let p1 = conn.placeholder(1);
    conn.execute(
        &format!(
            "UPDATE {} SET _latest = 0 WHERE _parent = {p1} AND _latest = 1",
            table
        ),
        &[DbValue::Text(parent_id.to_string())],
    )
    .context("Failed to clear previous latest flag")?;

    // Insert new version
    let snapshot_str = serde_json::to_string(snapshot).context("Failed to serialize snapshot")?;
    let (p1, p2, p3, p4, p5) = (
        conn.placeholder(1),
        conn.placeholder(2),
        conn.placeholder(3),
        conn.placeholder(4),
        conn.placeholder(5),
    );
    conn.execute(
        &format!(
            "INSERT INTO {} (id, _parent, _version, _status, _latest, snapshot) VALUES ({p1}, {p2}, {p3}, {p4}, 1, {p5})",
            table
        ),
        &[
            DbValue::Text(id.clone()),
            DbValue::Text(parent_id.to_string()),
            DbValue::Integer(next_version),
            DbValue::Text(status.to_string()),
            DbValue::Text(snapshot_str),
        ],
    )
    .context("Failed to insert version")?;

    Ok(VersionSnapshot::builder(id, parent_id)
        .version(next_version)
        .status(status)
        .latest(true)
        .snapshot(snapshot.clone())
        .build())
}

/// Find the latest version for a parent document.
pub fn find_latest_version(
    conn: &dyn DbConnection,
    slug: &str,
    parent_id: &str,
) -> Result<Option<VersionSnapshot>> {
    let table = format!("_versions_{}", slug);
    let p1 = conn.placeholder(1);
    let sql = format!(
        "SELECT id, _parent, _version, _status, _latest, snapshot, created_at, updated_at \
             FROM {} WHERE _parent = {p1} AND _latest = 1 LIMIT 1",
        table
    );

    match conn.query_one(&sql, &[DbValue::Text(parent_id.to_string())])? {
        Some(row) => {
            let snapshot_str = row.get_string("snapshot")?;
            let id = row.get_string("id")?;
            let parent = row.get_string("_parent")?;
            let version = row.get_i64("_version")?;
            let status = row.get_string("_status")?;
            let latest = row.get_bool("_latest")?;
            Ok(Some(
                VersionSnapshot::builder(id, parent)
                    .version(version)
                    .status(status)
                    .latest(latest)
                    .snapshot(
                        serde_json::from_str(&snapshot_str)
                            .context("Failed to parse version snapshot JSON")?,
                    )
                    .build(),
            ))
        }
        None => Ok(None),
    }
}

/// Count total versions for a parent document.
pub fn count_versions(conn: &dyn DbConnection, slug: &str, parent_id: &str) -> Result<i64> {
    let table = format!("_versions_{}", slug);
    let p1 = conn.placeholder(1);
    let row = conn
        .query_one(
            &format!("SELECT COUNT(*) AS cnt FROM {} WHERE _parent = {p1}", table),
            &[DbValue::Text(parent_id.to_string())],
        )?
        .context("Failed to count versions")?;
    row.get_i64("cnt")
}

/// List versions for a parent document, newest first.
pub fn list_versions(
    conn: &dyn DbConnection,
    slug: &str,
    parent_id: &str,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<Vec<VersionSnapshot>> {
    let table = format!("_versions_{}", slug);
    let p1 = conn.placeholder(1);
    let mut params: Vec<DbValue> = vec![DbValue::Text(parent_id.to_string())];
    let mut idx = 2;

    let limit_clause = match limit {
        Some(l) => {
            let p = conn.placeholder(idx);
            params.push(DbValue::Integer(l));
            idx += 1;
            format!(" LIMIT {p}")
        }
        None => String::new(),
    };
    let offset_clause = match offset {
        Some(o) => {
            let p = conn.placeholder(idx);
            params.push(DbValue::Integer(o));
            format!(" OFFSET {p}")
        }
        None => String::new(),
    };

    let sql = format!(
        "SELECT id, _parent, _version, _status, _latest, snapshot, created_at, updated_at \
             FROM {} WHERE _parent = {p1} ORDER BY _version DESC{}{}",
        table, limit_clause, offset_clause
    );

    let rows = conn.query_all(&sql, &params)?;
    let mut versions = Vec::new();
    for row in rows {
        let snapshot_str = row.get_string("snapshot")?;
        let id = row.get_string("id")?;
        let parent = row.get_string("_parent")?;
        let version = row.get_i64("_version")?;
        let status = row.get_string("_status")?;
        let latest = row.get_bool("_latest")?;
        versions.push(
            VersionSnapshot::builder(id, parent)
                .version(version)
                .status(status)
                .latest(latest)
                .snapshot(
                    serde_json::from_str(&snapshot_str)
                        .context("Failed to parse version snapshot JSON")?,
                )
                .build(),
        );
    }
    Ok(versions)
}

/// Find a specific version by its ID.
pub fn find_version_by_id(
    conn: &dyn DbConnection,
    slug: &str,
    version_id: &str,
) -> Result<Option<VersionSnapshot>> {
    let table = format!("_versions_{}", slug);
    let p1 = conn.placeholder(1);
    let sql = format!(
        "SELECT id, _parent, _version, _status, _latest, snapshot, created_at, updated_at \
             FROM {} WHERE id = {p1} LIMIT 1",
        table
    );

    match conn.query_one(&sql, &[DbValue::Text(version_id.to_string())])? {
        Some(row) => {
            let snapshot_str = row.get_string("snapshot")?;
            let id = row.get_string("id")?;
            let parent = row.get_string("_parent")?;
            let version = row.get_i64("_version")?;
            let status = row.get_string("_status")?;
            let latest = row.get_bool("_latest")?;
            Ok(Some(
                VersionSnapshot::builder(id, parent)
                    .version(version)
                    .status(status)
                    .latest(latest)
                    .snapshot(
                        serde_json::from_str(&snapshot_str)
                            .context("Failed to parse version snapshot JSON")?,
                    )
                    .build(),
            ))
        }
        None => Ok(None),
    }
}

/// Delete oldest versions beyond the max_versions cap for a document.
pub fn prune_versions(
    conn: &dyn DbConnection,
    slug: &str,
    parent_id: &str,
    max_versions: u32,
) -> Result<()> {
    if max_versions == 0 {
        return Ok(()); // unlimited
    }
    let table = format!("_versions_{}", slug);
    // Delete all versions beyond the cap, keeping the newest ones
    let (p1, p2) = (conn.placeholder(1), conn.placeholder(2));
    conn.execute(
        &format!(
            "DELETE FROM {} WHERE _parent = {p1} AND id NOT IN (\
                SELECT id FROM {} WHERE _parent = {p1} ORDER BY _version DESC LIMIT {p2}\
            )",
            table, table
        ),
        &[
            DbValue::Text(parent_id.to_string()),
            DbValue::Integer(max_versions as i64),
        ],
    )
    .context("Failed to prune versions")?;
    Ok(())
}

/// Set the `_status` column on a document in the main table.
pub fn set_document_status(
    conn: &dyn DbConnection,
    slug: &str,
    id: &str,
    status: &str,
) -> Result<()> {
    let (p1, p2) = (conn.placeholder(1), conn.placeholder(2));
    conn.execute(
        &format!(
            "UPDATE {} SET _status = {p1}, updated_at = {} WHERE id = {p2}",
            slug,
            conn.now_expr()
        ),
        &[
            DbValue::Text(status.to_string()),
            DbValue::Text(id.to_string()),
        ],
    )
    .with_context(|| format!("Failed to set _status on {}.{}", slug, id))?;
    Ok(())
}

/// Get the `_status` column from a document in the main table.
pub fn get_document_status(
    conn: &dyn DbConnection,
    slug: &str,
    id: &str,
) -> Result<Option<String>> {
    let p1 = conn.placeholder(1);
    match conn.query_one(
        &format!("SELECT _status FROM {} WHERE id = {p1}", slug),
        &[DbValue::Text(id.to_string())],
    )? {
        Some(row) => Ok(row.get_opt_string("_status")?),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::config::CrapConfig;
    use crate::db::{BoxedConnection, pool};
    use tempfile::TempDir;

    fn setup_versions_db() -> (TempDir, BoxedConnection) {
        let dir = TempDir::new().unwrap();
        let config = CrapConfig::default();
        let db_pool = pool::create_pool(dir.path(), &config).unwrap();
        let conn = db_pool.get().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                title TEXT,
                _status TEXT DEFAULT 'published',
                created_at TEXT DEFAULT (datetime('now')),
                updated_at TEXT DEFAULT (datetime('now'))
            );
            CREATE TABLE _versions_posts (
                id TEXT PRIMARY KEY,
                _parent TEXT NOT NULL,
                _version INTEGER NOT NULL,
                _status TEXT NOT NULL,
                _latest INTEGER NOT NULL DEFAULT 0,
                snapshot TEXT NOT NULL,
                created_at TEXT DEFAULT (datetime('now')),
                updated_at TEXT DEFAULT (datetime('now'))
            );
            INSERT INTO posts (id, title, _status) VALUES ('p1', 'Hello', 'published');",
        )
        .unwrap();
        (dir, conn)
    }

    #[test]
    fn create_and_find_latest_version() {
        let (_dir, conn) = setup_versions_db();
        let snapshot = json!({"title": "Hello"});

        let v = create_version(&conn, "posts", "p1", "published", &snapshot).unwrap();
        assert_eq!(v.parent, "p1");
        assert_eq!(v.version, 1);
        assert_eq!(v.status, "published");
        assert!(v.latest);
        assert_eq!(v.snapshot, snapshot);

        let latest = find_latest_version(&conn, "posts", "p1").unwrap();
        assert!(latest.is_some());
        let latest = latest.unwrap();
        assert_eq!(latest.version, 1);
        assert!(latest.latest);
    }

    #[test]
    fn create_multiple_versions_latest_flag() {
        let (_dir, conn) = setup_versions_db();

        let v1 =
            create_version(&conn, "posts", "p1", "published", &json!({"title": "V1"})).unwrap();
        assert_eq!(v1.version, 1);

        let v2 = create_version(&conn, "posts", "p1", "draft", &json!({"title": "V2"})).unwrap();
        assert_eq!(v2.version, 2);
        assert!(v2.latest);

        // v1 should no longer be latest
        let v1_refetched = find_version_by_id(&conn, "posts", &v1.id).unwrap().unwrap();
        assert!(!v1_refetched.latest, "v1 should no longer be latest");

        let latest = find_latest_version(&conn, "posts", "p1").unwrap().unwrap();
        assert_eq!(latest.version, 2);
    }

    #[test]
    fn find_latest_version_none() {
        let (_dir, conn) = setup_versions_db();
        let result = find_latest_version(&conn, "posts", "nonexistent").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn count_versions_empty_and_populated() {
        let (_dir, conn) = setup_versions_db();
        assert_eq!(count_versions(&conn, "posts", "p1").unwrap(), 0);

        create_version(&conn, "posts", "p1", "published", &json!({})).unwrap();
        assert_eq!(count_versions(&conn, "posts", "p1").unwrap(), 1);

        create_version(&conn, "posts", "p1", "draft", &json!({})).unwrap();
        assert_eq!(count_versions(&conn, "posts", "p1").unwrap(), 2);
    }

    #[test]
    fn list_versions_order_and_pagination() {
        let (_dir, conn) = setup_versions_db();
        for i in 0..5 {
            create_version(&conn, "posts", "p1", "published", &json!({"v": i})).unwrap();
        }

        // List all, newest first
        let all = list_versions(&conn, "posts", "p1", None, None).unwrap();
        assert_eq!(all.len(), 5);
        assert_eq!(all[0].version, 5); // newest first
        assert_eq!(all[4].version, 1);

        // Limit
        let limited = list_versions(&conn, "posts", "p1", Some(2), None).unwrap();
        assert_eq!(limited.len(), 2);
        assert_eq!(limited[0].version, 5);
        assert_eq!(limited[1].version, 4);

        // Offset
        let offset = list_versions(&conn, "posts", "p1", Some(2), Some(2)).unwrap();
        assert_eq!(offset.len(), 2);
        assert_eq!(offset[0].version, 3);
        assert_eq!(offset[1].version, 2);
    }

    #[test]
    fn find_version_by_id_found_and_not_found() {
        let (_dir, conn) = setup_versions_db();
        let v =
            create_version(&conn, "posts", "p1", "published", &json!({"title": "Test"})).unwrap();

        let found = find_version_by_id(&conn, "posts", &v.id).unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().id, v.id);

        let missing = find_version_by_id(&conn, "posts", "nonexistent").unwrap();
        assert!(missing.is_none());
    }

    #[test]
    fn set_and_get_document_status() {
        let (_dir, conn) = setup_versions_db();

        let status = get_document_status(&conn, "posts", "p1").unwrap();
        assert_eq!(status, Some("published".to_string()));

        set_document_status(&conn, "posts", "p1", "draft").unwrap();
        let status = get_document_status(&conn, "posts", "p1").unwrap();
        assert_eq!(status, Some("draft".to_string()));
    }

    #[test]
    fn get_document_status_not_found() {
        let (_dir, conn) = setup_versions_db();
        let status = get_document_status(&conn, "posts", "nonexistent").unwrap();
        assert!(status.is_none());
    }

    #[test]
    fn malformed_snapshot_json_returns_error() {
        let (_dir, conn) = setup_versions_db();

        // Insert a version row with corrupt snapshot JSON directly
        conn.execute_batch(
            "INSERT INTO _versions_posts (id, _parent, _version, _status, _latest, snapshot) \
             VALUES ('bad1', 'p1', 1, 'published', 1, '{not valid json!')",
        )
        .unwrap();

        let result = find_latest_version(&conn, "posts", "p1");
        assert!(
            result.is_err(),
            "Malformed snapshot JSON should return an error"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("Failed to parse version snapshot JSON"),
            "Error should mention snapshot parsing, got: {msg}"
        );
    }

    #[test]
    fn prune_versions_unlimited() {
        let (_dir, conn) = setup_versions_db();
        for _ in 0..5 {
            create_version(&conn, "posts", "p1", "published", &json!({})).unwrap();
        }
        // max_versions = 0 means unlimited -- should not delete anything
        prune_versions(&conn, "posts", "p1", 0).unwrap();
        assert_eq!(count_versions(&conn, "posts", "p1").unwrap(), 5);
    }

    #[test]
    fn prune_versions_caps() {
        let (_dir, conn) = setup_versions_db();
        for _ in 0..5 {
            create_version(&conn, "posts", "p1", "published", &json!({})).unwrap();
        }
        prune_versions(&conn, "posts", "p1", 3).unwrap();
        assert_eq!(count_versions(&conn, "posts", "p1").unwrap(), 3);

        // The remaining should be the 3 newest
        let remaining = list_versions(&conn, "posts", "p1", None, None).unwrap();
        assert_eq!(remaining[0].version, 5);
        assert_eq!(remaining[2].version, 3);
    }
}
