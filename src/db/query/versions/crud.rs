//! Version CRUD operations and document status management.

use anyhow::{Context as _, Result};
use nanoid::nanoid;
use serde_json::Value;

use crate::{
    core::{Builder, document::VersionSnapshot},
    db::{
        DbConnection, DbRow, DbValue,
        query::helpers::{
            SOFT_DELETE_ACTIVE, floor_optional_limit, quote_ident, utc_now, versions_table,
        },
    },
};

/// Build the quoted version table name for a collection slug. The name scheme
/// lives in the shared [`versions_table`]; this just quotes it for interpolation.
fn version_table(slug: &str) -> String {
    quote_ident(&versions_table(slug))
}

/// Map a database row to a `VersionSnapshot`.
fn row_to_version(row: &DbRow) -> Result<VersionSnapshot> {
    let snapshot_str = row.get_string("snapshot")?;

    Ok(
        VersionSnapshot::builder(row.get_string("id")?, row.get_string("_parent")?)
            .version(row.get_i64("_version")?)
            .status(row.get_string("_status")?)
            .latest(row.get_bool("_latest")?)
            .snapshot(
                serde_json::from_str(&snapshot_str)
                    .context("Failed to parse version snapshot JSON")?,
            )
            // Every consumer renders this (admin sidebar/table, Lua
            // `list_versions`, the gRPC codec); selecting it is what makes it
            // more than an empty string.
            .maybe_created_at(row.get_string("created_at").ok())
            .build(),
    )
}

/// Create a new version entry. Clears previous `_latest` flag, inserts new version.
///
/// # Errors
///
/// Returns a backend error if any SELECT/UPDATE/INSERT fails or the snapshot
/// fails to serialize.
pub fn create_version(
    conn: &dyn DbConnection,
    slug: &str,
    parent_id: &str,
    status: &str,
    snapshot: &Value,
) -> Result<VersionSnapshot> {
    let table = version_table(slug);
    let id = nanoid!();

    let p1 = conn.placeholder(1);
    let next_version: i64 = conn
        .query_one(
            &format!("SELECT COALESCE(MAX(_version), 0) + 1 AS next_ver FROM {table} WHERE _parent = {p1}"),
            &[DbValue::Text(parent_id.to_string())],
        )?
        .map(|row| row.get_i64("next_ver"))
        .transpose()?
        .unwrap_or(1);

    let p1 = conn.placeholder(1);
    conn.execute(
        &format!("UPDATE {table} SET _latest = 0 WHERE _parent = {p1} AND _latest = 1"),
        &[DbValue::Text(parent_id.to_string())],
    )
    .context("Failed to clear previous latest flag")?;

    let snapshot_str = serde_json::to_string(snapshot).context("Failed to serialize snapshot")?;
    // `created_at` is bound explicitly: a version table created by an early
    // release keeps its `datetime('now')` column default (version tables are
    // never altered), which stores the space-separated legacy format. Only
    // `created_at` is bound because it is the one timestamp column every
    // version table has always had.
    let now = utc_now();
    let (p1, p2, p3, p4, p5, p6) = (
        conn.placeholder(1),
        conn.placeholder(2),
        conn.placeholder(3),
        conn.placeholder(4),
        conn.placeholder(5),
        conn.placeholder(6),
    );
    conn.execute(
        &format!("INSERT INTO {table} (id, _parent, _version, _status, _latest, snapshot, created_at) VALUES ({p1}, {p2}, {p3}, {p4}, 1, {p5}, {p6})"),
        &[
            DbValue::Text(id.clone()),
            DbValue::Text(parent_id.to_string()),
            DbValue::Integer(next_version),
            DbValue::Text(status.to_string()),
            DbValue::Text(snapshot_str),
            DbValue::Text(now),
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

/// One version row a lifecycle step writes, together with the cap the
/// document's history is pruned to once it lands.
#[derive(Builder)]
pub struct VersionWrite<'a> {
    /// The parent's table: a collection slug, or a global's table name.
    #[builder(required)]
    pub slug: &'a str,
    #[builder(required)]
    pub parent_id: &'a str,
    /// The status this version is stamped with — `"published"` or `"draft"`.
    #[builder(required)]
    pub status: &'a str,
    #[builder(required)]
    pub snapshot: &'a Value,
    /// `0` keeps every version (see [`crate::core::VersionsConfig::cap`]).
    pub max_versions: u32,
}

/// Write one version row and prune the document's history to its cap.
///
/// The ONE step every lifecycle that records a version goes through — create,
/// update, draft save, unpublish, restore — so none of them can record history
/// without also honoring `max_versions`. Restore used to create a version and
/// never prune, so repeated restores grew the table without bound.
///
/// The parent row is locked first. `_version` is `MAX(_version) + 1` read on an
/// unlocked SELECT, so two concurrent writers on one document would otherwise
/// compute the same number and the loser would fail the version table's unique
/// index with a raw backend error. `SQLite` serializes writers already, which is
/// exactly what makes [`DbConnection::lock_row`] a no-op there.
///
/// Pruning can remove the last snapshot that referenced a stored upload file.
/// Which bytes that releases is decided by difference over the whole document
/// (`service::write::settle_upload_write`), which needs the live row as well as
/// the snapshots — so every lifecycle step that reaches here runs inside that
/// bracket instead of deleting files from this layer.
///
/// # Errors
///
/// Returns a backend error if the row lock, the insert or the prune fails.
pub fn create_version_and_prune(
    conn: &dyn DbConnection,
    write: &VersionWrite<'_>,
) -> Result<VersionSnapshot> {
    conn.lock_row(write.slug, write.parent_id)?;

    let version = create_version(
        conn,
        write.slug,
        write.parent_id,
        write.status,
        write.snapshot,
    )?;

    prune_versions(conn, write.slug, write.parent_id, write.max_versions)?;

    Ok(version)
}

/// Find the latest version for a parent document.
///
/// # Errors
///
/// Returns a backend error if the SELECT fails or the row fails to parse.
pub fn find_latest_version(
    conn: &dyn DbConnection,
    slug: &str,
    parent_id: &str,
) -> Result<Option<VersionSnapshot>> {
    let table = version_table(slug);
    let p1 = conn.placeholder(1);
    let sql = format!(
        "SELECT id, _parent, _version, _status, _latest, snapshot, created_at \
         FROM {table} WHERE _parent = {p1} AND _latest = 1 LIMIT 1"
    );

    let Some(row) = conn.query_one(&sql, &[DbValue::Text(parent_id.to_string())])? else {
        return Ok(None);
    };

    Ok(Some(row_to_version(&row)?))
}

/// Find the most recent *published* version snapshot for a parent (the highest
/// `_version` whose `_status` is `"published"`), if any.
///
/// Unlike [`find_latest_version`] this ignores the `_latest` flag — after an
/// unpublish the latest version is a draft, but the last published snapshot is
/// still needed to serve published-only readers.
///
/// # Errors
///
/// Returns a backend error if the SELECT fails or the row fails to parse.
pub fn find_latest_published_version(
    conn: &dyn DbConnection,
    slug: &str,
    parent_id: &str,
) -> Result<Option<VersionSnapshot>> {
    let table = version_table(slug);
    let p1 = conn.placeholder(1);
    let sql = format!(
        "SELECT id, _parent, _version, _status, _latest, snapshot, created_at \
         FROM {table} WHERE _parent = {p1} AND _status = 'published' \
         ORDER BY _version DESC LIMIT 1"
    );

    let Some(row) = conn.query_one(&sql, &[DbValue::Text(parent_id.to_string())])? else {
        return Ok(None);
    };

    Ok(Some(row_to_version(&row)?))
}

/// The `_status = 'published'` clause appended when a caller may only see
/// published versions (a reader without draft/edit access). `'published'` is a
/// fixed literal, so there is no parameter to bind.
fn published_only_clause(published_only: bool) -> &'static str {
    if published_only {
        " AND _status = 'published'"
    } else {
        ""
    }
}

/// Count versions for a parent document. When `published_only` is true, only
/// `_status = 'published'` snapshots are counted (draft versions are edit-gated).
///
/// # Errors
///
/// Returns a backend error if the COUNT query fails.
pub fn count_versions(
    conn: &dyn DbConnection,
    slug: &str,
    parent_id: &str,
    published_only: bool,
) -> Result<i64> {
    let table = version_table(slug);
    let p1 = conn.placeholder(1);
    let status = published_only_clause(published_only);
    let row = conn
        .query_one(
            &format!("SELECT COUNT(*) AS cnt FROM {table} WHERE _parent = {p1}{status}"),
            &[DbValue::Text(parent_id.to_string())],
        )?
        .context("Failed to count versions")?;
    row.get_i64("cnt")
}

/// List versions for a parent document, newest first. When `published_only` is
/// true, only `_status = 'published'` snapshots are returned.
///
/// # Errors
///
/// Returns a backend error if the SELECT fails or any row fails to parse.
pub fn list_versions(
    conn: &dyn DbConnection,
    slug: &str,
    parent_id: &str,
    published_only: bool,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<Vec<VersionSnapshot>> {
    let table = version_table(slug);
    let p1 = conn.placeholder(1);
    let status = published_only_clause(published_only);
    let mut params: Vec<DbValue> = vec![DbValue::Text(parent_id.to_string())];
    let mut idx = 2;

    // Floor at the chokepoint so no surface can smuggle `LIMIT -1` (no limit
    // in `SQLite` — a fail-open bypass) or a negative OFFSET past us.
    let limit_clause = match floor_optional_limit(limit) {
        Some(l) => {
            let p = conn.placeholder(idx);
            params.push(DbValue::Integer(l));
            idx += 1;
            format!(" LIMIT {p}")
        }
        None => String::new(),
    };

    let offset_clause = match floor_optional_limit(offset) {
        Some(o) => {
            let p = conn.placeholder(idx);
            params.push(DbValue::Integer(o));
            format!(" OFFSET {p}")
        }
        None => String::new(),
    };

    let sql = format!(
        "SELECT id, _parent, _version, _status, _latest, snapshot, created_at \
         FROM {table} WHERE _parent = {p1}{status} ORDER BY _version DESC{limit_clause}{offset_clause}"
    );

    conn.query_all(&sql, &params)?
        .iter()
        .map(row_to_version)
        .collect()
}

/// Every version snapshot stored for one document — the snapshot JSON only,
/// without the surrounding version metadata.
///
/// This is what answers "does anything still reference this file": a stored
/// upload file outlives the published row dropping it for as long as a draft or
/// version snapshot of the same document names it. The keys are derived from
/// each snapshot with the same `upload_file_entries` rule the live row goes
/// through, rather than matched as text, so the two sides cannot disagree about
/// what counts as a file reference.
///
/// # Errors
///
/// Returns a backend error if the SELECT fails or a snapshot fails to parse.
pub fn list_snapshots(conn: &dyn DbConnection, slug: &str, parent_id: &str) -> Result<Vec<Value>> {
    let table = version_table(slug);
    let p1 = conn.placeholder(1);

    conn.query_all(
        &format!("SELECT snapshot FROM {table} WHERE _parent = {p1}"),
        &[DbValue::Text(parent_id.to_string())],
    )?
    .iter()
    .map(|row| {
        serde_json::from_str(&row.get_string("snapshot")?)
            .context("Failed to parse version snapshot JSON")
    })
    .collect()
}

/// Find a specific version by its ID.
///
/// # Errors
///
/// Returns a backend error if the SELECT fails or the row fails to parse.
pub fn find_version_by_id(
    conn: &dyn DbConnection,
    slug: &str,
    version_id: &str,
) -> Result<Option<VersionSnapshot>> {
    let table = version_table(slug);
    let p1 = conn.placeholder(1);
    let sql = format!(
        "SELECT id, _parent, _version, _status, _latest, snapshot, created_at \
         FROM {table} WHERE id = {p1} LIMIT 1"
    );

    let Some(row) = conn.query_one(&sql, &[DbValue::Text(version_id.to_string())])? else {
        return Ok(None);
    };

    Ok(Some(row_to_version(&row)?))
}

/// Delete oldest versions beyond the `max_versions` cap for a document.
///
/// # Errors
///
/// Returns a backend error if the DELETE fails.
pub fn prune_versions(
    conn: &dyn DbConnection,
    slug: &str,
    parent_id: &str,
    max_versions: u32,
) -> Result<()> {
    if max_versions == 0 {
        return Ok(()); // unlimited
    }

    let table = version_table(slug);
    let (p1, p2) = (conn.placeholder(1), conn.placeholder(2));
    // The newest PUBLISHED snapshot is exempt from the cap: an unpublished
    // document serves it to published readers (`find_latest_published_version`),
    // so pruning it would silently empty that view while the row still holds
    // the content. Everything else falls off by version, newest kept.
    conn.execute(
        &format!(
            "DELETE FROM {table} WHERE _parent = {p1} AND id NOT IN (\
                SELECT id FROM {table} WHERE _parent = {p1} ORDER BY _version DESC LIMIT {p2}\
            ) AND id NOT IN (\
                SELECT id FROM {table} WHERE _parent = {p1} AND _status = 'published' \
                ORDER BY _version DESC LIMIT 1\
            )"
        ),
        &[
            DbValue::Text(parent_id.to_string()),
            DbValue::Integer(i64::from(max_versions)),
        ],
    )
    .context("Failed to prune versions")?;
    Ok(())
}

/// Set the `_status` column on a document in the main table.
///
/// # Errors
///
/// Returns a backend error if the UPDATE fails.
pub fn set_document_status(
    conn: &dyn DbConnection,
    slug: &str,
    id: &str,
    status: &str,
) -> Result<()> {
    let (p1, p2) = (conn.placeholder(1), conn.placeholder(2));
    conn.execute(
        &format!(
            "UPDATE \"{}\" SET _status = {p1}, updated_at = {} WHERE id = {p2}",
            slug,
            conn.now_expr()
        ),
        &[
            DbValue::Text(status.to_string()),
            DbValue::Text(id.to_string()),
        ],
    )
    .with_context(|| format!("Failed to set _status on {slug}.{id}"))?;
    Ok(())
}

/// Whether the document row is visible under the LIVE lifecycle — i.e. it
/// exists and is not soft-deleted. Used by the draft overlay, which bypasses
/// the SQL `WHERE` path and would otherwise serve a trashed document's draft
/// snapshot as a live document.
///
/// # Errors
///
/// Returns a backend error if the SELECT fails.
pub fn document_is_live(conn: &dyn DbConnection, slug: &str, id: &str) -> Result<bool> {
    let p1 = conn.placeholder(1);
    let sql = format!("SELECT 1 FROM \"{slug}\" WHERE id = {p1} AND {SOFT_DELETE_ACTIVE}");

    Ok(conn
        .query_one(&sql, &[DbValue::Text(id.to_string())])?
        .is_some())
}

/// Get the `_status` column from a document in the main table.
///
/// # Errors
///
/// Returns a backend error if the SELECT fails or column extraction fails.
pub fn get_document_status(
    conn: &dyn DbConnection,
    slug: &str,
    id: &str,
) -> Result<Option<String>> {
    let p1 = conn.placeholder(1);
    let Some(row) = conn.query_one(
        &format!("SELECT _status FROM \"{slug}\" WHERE id = {p1}"),
        &[DbValue::Text(id.to_string())],
    )?
    else {
        return Ok(None);
    };

    row.get_opt_string("_status")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::config::CrapConfig;
    use crate::db::{BoxedConnection, pool, query::test_helpers::CountingConn};
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
        assert_eq!(count_versions(&conn, "posts", "p1", false).unwrap(), 0);

        create_version(&conn, "posts", "p1", "published", &json!({})).unwrap();
        assert_eq!(count_versions(&conn, "posts", "p1", false).unwrap(), 1);

        create_version(&conn, "posts", "p1", "draft", &json!({})).unwrap();
        assert_eq!(count_versions(&conn, "posts", "p1", false).unwrap(), 2);
    }

    #[test]
    fn list_versions_order_and_pagination() {
        let (_dir, conn) = setup_versions_db();
        for i in 0..5 {
            create_version(&conn, "posts", "p1", "published", &json!({"v": i})).unwrap();
        }

        // List all, newest first
        let all = list_versions(&conn, "posts", "p1", false, None, None).unwrap();
        assert_eq!(all.len(), 5);
        assert_eq!(all[0].version, 5); // newest first
        assert_eq!(all[4].version, 1);

        // Limit
        let limited = list_versions(&conn, "posts", "p1", false, Some(2), None).unwrap();
        assert_eq!(limited.len(), 2);
        assert_eq!(limited[0].version, 5);
        assert_eq!(limited[1].version, 4);

        // Offset
        let offset = list_versions(&conn, "posts", "p1", false, Some(2), Some(2)).unwrap();
        assert_eq!(offset.len(), 2);
        assert_eq!(offset[0].version, 3);
        assert_eq!(offset[1].version, 2);
    }

    /// Regression: a negative limit was bound raw, and `LIMIT -1` means *no
    /// limit* in `SQLite` — a fail-open bypass of the caller's cap (the Lua
    /// surface passed its `Option<i64>` unfloored). Both values floor to 0
    /// at this chokepoint now, for every surface.
    #[test]
    fn list_versions_floors_negative_limit_and_offset() {
        let (_dir, conn) = setup_versions_db();
        for i in 0..3 {
            create_version(&conn, "posts", "p1", "published", &json!({"v": i})).unwrap();
        }

        let neg_limit = list_versions(&conn, "posts", "p1", false, Some(-1), None).unwrap();
        assert!(
            neg_limit.is_empty(),
            "LIMIT -1 must floor to 0, not disable the limit"
        );

        let neg_offset = list_versions(&conn, "posts", "p1", false, Some(2), Some(-3)).unwrap();
        assert_eq!(neg_offset.len(), 2, "negative offset must floor to 0");
        assert_eq!(neg_offset[0].version, 3);
    }

    /// Every snapshot of the named document comes back, and another
    /// document's snapshots stay out of it — file references are per-document.
    #[test]
    fn list_snapshots_returns_every_snapshot_of_one_document() {
        let (_dir, conn) = setup_versions_db();
        create_version(
            &conn,
            "posts",
            "p1",
            "published",
            &json!({"url": "/uploads/a.png"}),
        )
        .unwrap();
        create_version(
            &conn,
            "posts",
            "p1",
            "draft",
            &json!({"url": "/uploads/b.png"}),
        )
        .unwrap();
        create_version(
            &conn,
            "posts",
            "p2",
            "draft",
            &json!({"url": "/uploads/c.png"}),
        )
        .unwrap();

        let mut urls: Vec<String> = list_snapshots(&conn, "posts", "p1")
            .unwrap()
            .iter()
            .map(|s| s["url"].as_str().unwrap().to_string())
            .collect();
        urls.sort();

        assert_eq!(urls, vec!["/uploads/a.png", "/uploads/b.png"]);
        assert!(
            list_snapshots(&conn, "posts", "missing")
                .unwrap()
                .is_empty()
        );
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
        assert_eq!(count_versions(&conn, "posts", "p1", false).unwrap(), 5);
    }

    #[test]
    fn prune_versions_caps() {
        let (_dir, conn) = setup_versions_db();
        for _ in 0..5 {
            create_version(&conn, "posts", "p1", "published", &json!({})).unwrap();
        }
        prune_versions(&conn, "posts", "p1", 3).unwrap();
        assert_eq!(count_versions(&conn, "posts", "p1", false).unwrap(), 3);

        // The remaining should be the 3 newest
        let remaining = list_versions(&conn, "posts", "p1", false, None, None).unwrap();
        assert_eq!(remaining[0].version, 5);
        assert_eq!(remaining[2].version, 3);
    }

    /// The chokepoint every lifecycle step records history through prunes in
    /// the same breath, so no caller can grow the table past its cap. Restore
    /// created a version and never pruned.
    #[test]
    fn create_version_and_prune_caps_the_history_it_writes() {
        let (_dir, conn) = setup_versions_db();

        for i in 0..5 {
            create_version_and_prune(
                &conn,
                &VersionWrite::builder("posts", "p1", "draft", &json!({ "v": i }))
                    .max_versions(2)
                    .build(),
            )
            .unwrap();
        }

        assert_eq!(count_versions(&conn, "posts", "p1", false).unwrap(), 2);

        let remaining = list_versions(&conn, "posts", "p1", false, None, None).unwrap();
        assert_eq!(remaining[0].version, 5);
        assert_eq!(remaining[1].version, 4);
    }

    /// The next `_version` is `MAX(_version) + 1` read on an unlocked SELECT,
    /// so the parent row is locked first: two concurrent draft saves on one
    /// document would otherwise compute the same number and the loser would
    /// fail the version table's unique index with a raw backend error.
    #[test]
    fn create_version_and_prune_locks_the_parent_row_first() {
        let (_dir, conn) = setup_versions_db();
        let spy = CountingConn::new(&conn);

        create_version_and_prune(
            &spy,
            &VersionWrite::builder("posts", "p1", "draft", &json!({})).build(),
        )
        .unwrap();

        assert_eq!(
            spy.locks(),
            vec![("posts".to_string(), "p1".to_string())],
            "the parent row is locked before the version number is computed"
        );
    }

    /// A version table created with the legacy `datetime('now')` default still
    /// gets an ISO 8601 `created_at`: the insert binds it itself.
    #[test]
    fn create_version_writes_iso_timestamps_on_a_legacy_default_table() {
        let (_dir, conn) = setup_versions_db();

        let version = create_version(&conn, "posts", "p1", "published", &json!({})).unwrap();

        let row = conn
            .query_one(
                "SELECT created_at FROM _versions_posts WHERE id = ?1",
                &[DbValue::Text(version.id.clone())],
            )
            .unwrap()
            .expect("version row");
        let stamp = row.get_string("created_at").unwrap();
        assert!(
            stamp.contains('T') && stamp.ends_with('Z'),
            "created_at must be ISO 8601, got {stamp}"
        );
    }
}
