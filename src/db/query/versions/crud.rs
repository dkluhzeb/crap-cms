//! Version CRUD operations and document status management.

use anyhow::{Context as _, Result};

use crate::core::document::VersionSnapshot;

/// Create a new version entry. Clears previous `_latest` flag, inserts new version.
pub fn create_version(
    conn: &rusqlite::Connection,
    slug: &str,
    parent_id: &str,
    status: &str,
    snapshot: &serde_json::Value,
) -> Result<VersionSnapshot> {
    let table = format!("_versions_{}", slug);
    let id = nanoid::nanoid!();

    // Get the next version number
    let next_version: i64 = conn
        .query_row(
            &format!(
                "SELECT COALESCE(MAX(_version), 0) + 1 FROM {} WHERE _parent = ?1",
                table
            ),
            [parent_id],
            |row| row.get(0),
        )
        .context("Failed to get next version number")?;

    // Clear previous _latest flag
    conn.execute(
        &format!(
            "UPDATE {} SET _latest = 0 WHERE _parent = ?1 AND _latest = 1",
            table
        ),
        [parent_id],
    )
    .context("Failed to clear previous latest flag")?;

    // Insert new version
    let snapshot_str = serde_json::to_string(snapshot).context("Failed to serialize snapshot")?;
    conn.execute(
        &format!(
            "INSERT INTO {} (id, _parent, _version, _status, _latest, snapshot) VALUES (?1, ?2, ?3, ?4, 1, ?5)",
            table
        ),
        rusqlite::params![id, parent_id, next_version, status, snapshot_str],
    ).context("Failed to insert version")?;

    Ok(VersionSnapshot::builder(id, parent_id)
        .version(next_version)
        .status(status)
        .latest(true)
        .snapshot(snapshot.clone())
        .build())
}

/// Find the latest version for a parent document.
pub fn find_latest_version(
    conn: &rusqlite::Connection,
    slug: &str,
    parent_id: &str,
) -> Result<Option<VersionSnapshot>> {
    let table = format!("_versions_{}", slug);
    let mut stmt = conn.prepare(&format!(
        "SELECT id, _parent, _version, _status, _latest, snapshot, created_at, updated_at \
             FROM {} WHERE _parent = ?1 AND _latest = 1 LIMIT 1",
        table
    ))?;
    let result = stmt.query_row([parent_id], |row| {
        let snapshot_str: String = row.get(5)?;
        let id: String = row.get(0)?;
        let parent: String = row.get(1)?;
        Ok(VersionSnapshot::builder(id, parent)
            .version(row.get(2)?)
            .status(row.get::<_, String>(3)?)
            .latest(row.get::<_, i32>(4)? != 0)
            .snapshot(serde_json::from_str(&snapshot_str).unwrap_or(serde_json::Value::Null))
            .build())
    });

    match result {
        Ok(v) => Ok(Some(v)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Count total versions for a parent document.
pub fn count_versions(conn: &rusqlite::Connection, slug: &str, parent_id: &str) -> Result<i64> {
    let table = format!("_versions_{}", slug);
    let count: i64 = conn
        .query_row(
            &format!("SELECT COUNT(*) FROM {} WHERE _parent = ?1", table),
            [parent_id],
            |row| row.get(0),
        )
        .context("Failed to count versions")?;
    Ok(count)
}

/// List versions for a parent document, newest first.
pub fn list_versions(
    conn: &rusqlite::Connection,
    slug: &str,
    parent_id: &str,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<Vec<VersionSnapshot>> {
    let table = format!("_versions_{}", slug);
    let limit_clause = limit.map(|l| format!(" LIMIT {}", l)).unwrap_or_default();
    let offset_clause = offset.map(|o| format!(" OFFSET {}", o)).unwrap_or_default();
    let mut stmt = conn.prepare(&format!(
        "SELECT id, _parent, _version, _status, _latest, snapshot, created_at, updated_at \
             FROM {} WHERE _parent = ?1 ORDER BY _version DESC{}{}",
        table, limit_clause, offset_clause
    ))?;
    let rows = stmt.query_map([parent_id], |row| {
        let snapshot_str: String = row.get(5)?;
        let id: String = row.get(0)?;
        let parent: String = row.get(1)?;
        Ok(VersionSnapshot::builder(id, parent)
            .version(row.get(2)?)
            .status(row.get::<_, String>(3)?)
            .latest(row.get::<_, i32>(4)? != 0)
            .snapshot(serde_json::from_str(&snapshot_str).unwrap_or(serde_json::Value::Null))
            .build())
    })?;
    let mut versions = Vec::new();
    for row in rows {
        versions.push(row?);
    }
    Ok(versions)
}

/// Find a specific version by its ID.
pub fn find_version_by_id(
    conn: &rusqlite::Connection,
    slug: &str,
    version_id: &str,
) -> Result<Option<VersionSnapshot>> {
    let table = format!("_versions_{}", slug);
    let mut stmt = conn.prepare(&format!(
        "SELECT id, _parent, _version, _status, _latest, snapshot, created_at, updated_at \
             FROM {} WHERE id = ?1 LIMIT 1",
        table
    ))?;
    let result = stmt.query_row([version_id], |row| {
        let snapshot_str: String = row.get(5)?;
        let id: String = row.get(0)?;
        let parent: String = row.get(1)?;
        Ok(VersionSnapshot::builder(id, parent)
            .version(row.get(2)?)
            .status(row.get::<_, String>(3)?)
            .latest(row.get::<_, i32>(4)? != 0)
            .snapshot(serde_json::from_str(&snapshot_str).unwrap_or(serde_json::Value::Null))
            .build())
    });

    match result {
        Ok(v) => Ok(Some(v)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Delete oldest versions beyond the max_versions cap for a document.
pub fn prune_versions(
    conn: &rusqlite::Connection,
    slug: &str,
    parent_id: &str,
    max_versions: u32,
) -> Result<()> {
    if max_versions == 0 {
        return Ok(()); // unlimited
    }
    let table = format!("_versions_{}", slug);
    // Delete all versions beyond the cap, keeping the newest ones
    conn.execute(
        &format!(
            "DELETE FROM {} WHERE _parent = ?1 AND id NOT IN (\
                SELECT id FROM {} WHERE _parent = ?1 ORDER BY _version DESC LIMIT ?2\
            )",
            table, table
        ),
        rusqlite::params![parent_id, max_versions],
    )
    .context("Failed to prune versions")?;
    Ok(())
}

/// Set the `_status` column on a document in the main table.
pub fn set_document_status(
    conn: &rusqlite::Connection,
    slug: &str,
    id: &str,
    status: &str,
) -> Result<()> {
    conn.execute(
        &format!(
            "UPDATE {} SET _status = ?1, updated_at = datetime('now') WHERE id = ?2",
            slug
        ),
        rusqlite::params![status, id],
    )
    .with_context(|| format!("Failed to set _status on {}.{}", slug, id))?;
    Ok(())
}

/// Get the `_status` column from a document in the main table.
pub fn get_document_status(
    conn: &rusqlite::Connection,
    slug: &str,
    id: &str,
) -> Result<Option<String>> {
    let result = conn.query_row(
        &format!("SELECT _status FROM {} WHERE id = ?1", slug),
        [id],
        |row| row.get(0),
    );
    match result {
        Ok(status) => Ok(Some(status)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_versions_db() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
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
        conn
    }

    #[test]
    fn create_and_find_latest_version() {
        let conn = setup_versions_db();
        let snapshot = serde_json::json!({"title": "Hello"});

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
        let conn = setup_versions_db();

        let v1 = create_version(
            &conn,
            "posts",
            "p1",
            "published",
            &serde_json::json!({"title": "V1"}),
        )
        .unwrap();
        assert_eq!(v1.version, 1);

        let v2 = create_version(
            &conn,
            "posts",
            "p1",
            "draft",
            &serde_json::json!({"title": "V2"}),
        )
        .unwrap();
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
        let conn = setup_versions_db();
        let result = find_latest_version(&conn, "posts", "nonexistent").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn count_versions_empty_and_populated() {
        let conn = setup_versions_db();
        assert_eq!(count_versions(&conn, "posts", "p1").unwrap(), 0);

        create_version(&conn, "posts", "p1", "published", &serde_json::json!({})).unwrap();
        assert_eq!(count_versions(&conn, "posts", "p1").unwrap(), 1);

        create_version(&conn, "posts", "p1", "draft", &serde_json::json!({})).unwrap();
        assert_eq!(count_versions(&conn, "posts", "p1").unwrap(), 2);
    }

    #[test]
    fn list_versions_order_and_pagination() {
        let conn = setup_versions_db();
        for i in 0..5 {
            create_version(
                &conn,
                "posts",
                "p1",
                "published",
                &serde_json::json!({"v": i}),
            )
            .unwrap();
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
        let conn = setup_versions_db();
        let v = create_version(
            &conn,
            "posts",
            "p1",
            "published",
            &serde_json::json!({"title": "Test"}),
        )
        .unwrap();

        let found = find_version_by_id(&conn, "posts", &v.id).unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().id, v.id);

        let missing = find_version_by_id(&conn, "posts", "nonexistent").unwrap();
        assert!(missing.is_none());
    }

    #[test]
    fn set_and_get_document_status() {
        let conn = setup_versions_db();

        let status = get_document_status(&conn, "posts", "p1").unwrap();
        assert_eq!(status, Some("published".to_string()));

        set_document_status(&conn, "posts", "p1", "draft").unwrap();
        let status = get_document_status(&conn, "posts", "p1").unwrap();
        assert_eq!(status, Some("draft".to_string()));
    }

    #[test]
    fn get_document_status_not_found() {
        let conn = setup_versions_db();
        let status = get_document_status(&conn, "posts", "nonexistent").unwrap();
        assert!(status.is_none());
    }

    #[test]
    fn prune_versions_unlimited() {
        let conn = setup_versions_db();
        for _ in 0..5 {
            create_version(&conn, "posts", "p1", "published", &serde_json::json!({})).unwrap();
        }
        // max_versions = 0 means unlimited -- should not delete anything
        prune_versions(&conn, "posts", "p1", 0).unwrap();
        assert_eq!(count_versions(&conn, "posts", "p1").unwrap(), 5);
    }

    #[test]
    fn prune_versions_caps() {
        let conn = setup_versions_db();
        for _ in 0..5 {
            create_version(&conn, "posts", "p1", "published", &serde_json::json!({})).unwrap();
        }
        prune_versions(&conn, "posts", "p1", 3).unwrap();
        assert_eq!(count_versions(&conn, "posts", "p1").unwrap(), 3);

        // The remaining should be the 3 newest
        let remaining = list_versions(&conn, "posts", "p1", None, None).unwrap();
        assert_eq!(remaining[0].version, 5);
        assert_eq!(remaining[2].version, 3);
    }
}
