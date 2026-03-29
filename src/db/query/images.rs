//! Image processing queue database operations.

use anyhow::{Context as _, Result};

use crate::db::{DbConnection, DbRow, DbValue};

/// Extract a text value from a row by column index, returning empty string on mismatch/missing.
fn get_text(row: &DbRow, idx: usize) -> String {
    match row.get_value(idx) {
        Some(DbValue::Text(s)) => s.clone(),
        _ => String::new(),
    }
}

/// Extract an optional text value from a row by column index.
fn get_opt_text(row: &DbRow, idx: usize) -> Option<String> {
    match row.get_value(idx) {
        Some(DbValue::Text(s)) => Some(s.clone()),
        _ => None,
    }
}

/// A pending image format conversion entry.
#[derive(Debug, Clone)]
pub struct ImageQueueEntry {
    pub id: String,
    pub collection: String,
    pub document_id: String,
    pub source_path: String,
    pub target_path: String,
    pub format: String,
    pub quality: u8,
    pub url_column: String,
    pub url_value: String,
}

/// Parameters for inserting a new image queue entry.
pub struct NewImageEntry<'a> {
    pub collection: &'a str,
    pub document_id: &'a str,
    pub source_path: &'a str,
    pub target_path: &'a str,
    pub format: &'a str,
    pub quality: u8,
    pub url_column: &'a str,
    pub url_value: &'a str,
}

/// Summary entry for listing queue contents.
#[derive(Debug, Clone)]
pub struct ImageQueueListEntry {
    pub id: String,
    pub collection: String,
    pub document_id: String,
    pub format: String,
    pub status: String,
    pub error: Option<String>,
    pub created_at: Option<String>,
    pub completed_at: Option<String>,
}

/// Insert a pending image conversion into the queue.
pub fn insert_image_queue_entry(
    conn: &dyn DbConnection,
    entry: &NewImageEntry<'_>,
) -> Result<String> {
    let id = nanoid::nanoid!();
    let p1 = conn.placeholder(1);
    let p2 = conn.placeholder(2);
    let p3 = conn.placeholder(3);
    let p4 = conn.placeholder(4);
    let p5 = conn.placeholder(5);
    let p6 = conn.placeholder(6);
    let p7 = conn.placeholder(7);
    let p8 = conn.placeholder(8);
    let p9 = conn.placeholder(9);
    conn.execute(
        &format!(
            "INSERT INTO _crap_image_queue (id, collection, document_id, source_path, target_path, format, quality, url_column, url_value)
         VALUES ({p1}, {p2}, {p3}, {p4}, {p5}, {p6}, {p7}, {p8}, {p9})"
        ),
        &[
            DbValue::Text(id.clone()),
            DbValue::Text(entry.collection.to_string()),
            DbValue::Text(entry.document_id.to_string()),
            DbValue::Text(entry.source_path.to_string()),
            DbValue::Text(entry.target_path.to_string()),
            DbValue::Text(entry.format.to_string()),
            DbValue::Integer(entry.quality as i64),
            DbValue::Text(entry.url_column.to_string()),
            DbValue::Text(entry.url_value.to_string()),
        ],
    )
    .context("Failed to insert image queue entry")?;
    Ok(id)
}

/// Claim up to `limit` pending entries for processing.
///
/// Uses a two-phase approach:
/// 1. Atomically UPDATE pending rows to `'processing'` using a subquery that
///    limits to the oldest N entries and requires `status = 'pending'`.
///    Even if two callers SELECT the same pending IDs, the `AND status = 'pending'`
///    guard ensures only one UPDATE per row succeeds.
/// 2. Read back the specific IDs that were updated.
///
/// The returned count matches `execute`'s affected-row count, ensuring no
/// double-claiming.
pub fn claim_pending_images(conn: &dyn DbConnection, limit: usize) -> Result<Vec<ImageQueueEntry>> {
    let p1 = conn.placeholder(1);

    // Step 1: Find the candidate pending IDs.
    let id_rows = conn.query_all(
        &format!(
            "SELECT id FROM _crap_image_queue
             WHERE status = 'pending'
             ORDER BY created_at ASC
             LIMIT {p1}"
        ),
        &[DbValue::Integer(limit as i64)],
    )?;

    if id_rows.is_empty() {
        return Ok(Vec::new());
    }

    // Step 2: Atomically claim each row (only if still pending).
    let mut claimed_ids = Vec::with_capacity(id_rows.len());
    let p1 = conn.placeholder(1);

    for row in &id_rows {
        let id = get_text(row, 0);
        let updated = conn.execute(
            &format!(
                "UPDATE _crap_image_queue SET status = 'processing'
                 WHERE id = {p1} AND status = 'pending'"
            ),
            &[DbValue::Text(id.clone())],
        )?;

        if updated > 0 {
            claimed_ids.push(id);
        }
    }

    if claimed_ids.is_empty() {
        return Ok(Vec::new());
    }

    // Step 3: Read back only the rows we successfully claimed.
    let placeholders: Vec<String> = (0..claimed_ids.len())
        .map(|i| conn.placeholder(i + 1))
        .collect();
    let params: Vec<DbValue> = claimed_ids
        .iter()
        .map(|id| DbValue::Text(id.clone()))
        .collect();

    let rows = conn.query_all(
        &format!(
            "SELECT id, collection, document_id, source_path, target_path, format, quality, url_column, url_value
             FROM _crap_image_queue
             WHERE id IN ({})",
            placeholders.join(", ")
        ),
        &params,
    )?;

    let mut entries = Vec::with_capacity(rows.len());
    for row in &rows {
        let quality = match row.get_value(6) {
            Some(DbValue::Integer(n)) => *n as u8,
            _ => 0,
        };
        entries.push(ImageQueueEntry {
            id: get_text(row, 0),
            collection: get_text(row, 1),
            document_id: get_text(row, 2),
            source_path: get_text(row, 3),
            target_path: get_text(row, 4),
            format: get_text(row, 5),
            quality,
            url_column: get_text(row, 7),
            url_value: get_text(row, 8),
        });
    }

    Ok(entries)
}

/// Mark an entry as completed.
pub fn complete_image_entry(conn: &dyn DbConnection, id: &str) -> Result<()> {
    let p1 = conn.placeholder(1);
    conn.execute(
        &format!(
            "UPDATE _crap_image_queue SET status = 'completed', completed_at = {} WHERE id = {p1}",
            conn.now_expr()
        ),
        &[DbValue::Text(id.to_string())],
    )?;
    Ok(())
}

/// Mark an entry as failed with an error message.
pub fn fail_image_entry(conn: &dyn DbConnection, id: &str, error: &str) -> Result<()> {
    let p1 = conn.placeholder(1);
    let p2 = conn.placeholder(2);
    conn.execute(
        &format!("UPDATE _crap_image_queue SET status = 'failed', error = {p2}, completed_at = {} WHERE id = {p1}", conn.now_expr()),
        &[DbValue::Text(id.to_string()), DbValue::Text(error.to_string())],
    )?;
    Ok(())
}

/// Reset stale `processing` entries back to `pending`.
/// These are entries that were claimed but never completed — e.g., due to a server
/// crash mid-conversion. Called on scheduler startup.
pub fn recover_stale_images(conn: &dyn DbConnection) -> Result<i64> {
    let updated = conn.execute(
        "UPDATE _crap_image_queue SET status = 'pending' WHERE status = 'processing'",
        &[],
    )?;
    Ok(updated as i64)
}

/// Count pending entries in the queue.
pub fn count_pending_images(conn: &dyn DbConnection) -> Result<i64> {
    let row = conn
        .query_one(
            "SELECT COUNT(*) FROM _crap_image_queue WHERE status = 'pending'",
            &[],
        )?
        .context("Expected a count row")?;
    match row.get_value(0) {
        Some(DbValue::Integer(n)) => Ok(*n),
        _ => Ok(0),
    }
}

/// Count entries by status.
pub fn count_image_entries_by_status(conn: &dyn DbConnection, status: &str) -> Result<i64> {
    let p1 = conn.placeholder(1);
    let row = conn
        .query_one(
            &format!("SELECT COUNT(*) FROM _crap_image_queue WHERE status = {p1}"),
            &[DbValue::Text(status.to_string())],
        )?
        .context("Expected a count row")?;
    match row.get_value(0) {
        Some(DbValue::Integer(n)) => Ok(*n),
        _ => Ok(0),
    }
}

/// List queue entries with optional status filter and limit.
pub fn list_image_entries(
    conn: &dyn DbConnection,
    status_filter: Option<&str>,
    limit: i64,
) -> Result<Vec<ImageQueueListEntry>> {
    let p1 = conn.placeholder(1);
    let p2 = conn.placeholder(2);
    let (sql, params) = match status_filter {
        Some(status) => (
            format!(
                "SELECT id, collection, document_id, format, status, error, created_at, completed_at \
             FROM _crap_image_queue WHERE status = {p1} ORDER BY created_at DESC LIMIT {p2}"
            ),
            vec![DbValue::Text(status.to_string()), DbValue::Integer(limit)],
        ),
        None => (
            format!(
                "SELECT id, collection, document_id, format, status, error, created_at, completed_at \
             FROM _crap_image_queue ORDER BY created_at DESC LIMIT {p1}"
            ),
            vec![DbValue::Integer(limit)],
        ),
    };

    let rows = conn.query_all(&sql, &params)?;
    let entries = rows
        .iter()
        .map(|row| ImageQueueListEntry {
            id: get_text(row, 0),
            collection: get_text(row, 1),
            document_id: get_text(row, 2),
            format: get_text(row, 3),
            status: get_text(row, 4),
            error: get_opt_text(row, 5),
            created_at: get_opt_text(row, 6),
            completed_at: get_opt_text(row, 7),
        })
        .collect();

    Ok(entries)
}

/// Reset a single failed entry to pending for retry.
pub fn retry_image_entry(conn: &dyn DbConnection, id: &str) -> Result<bool> {
    let p1 = conn.placeholder(1);
    let updated = conn.execute(
        &format!(
            "UPDATE _crap_image_queue SET status = 'pending', error = NULL, completed_at = NULL \
         WHERE id = {p1} AND status = 'failed'"
        ),
        &[DbValue::Text(id.to_string())],
    )?;
    Ok(updated > 0)
}

/// Reset all failed entries to pending. Returns the count of reset entries.
pub fn retry_all_failed_images(conn: &dyn DbConnection) -> Result<i64> {
    let updated = conn.execute(
        "UPDATE _crap_image_queue SET status = 'pending', error = NULL, completed_at = NULL \
         WHERE status = 'failed'",
        &[],
    )?;
    Ok(updated as i64)
}

/// Purge completed/failed entries older than the given number of seconds.
pub fn purge_old_image_entries(conn: &dyn DbConnection, older_than_secs: u64) -> Result<i64> {
    let (offset_sql, offset_param) = conn.date_offset_expr(older_than_secs as i64, 1);
    let deleted = conn.execute(
        &format!(
            "DELETE FROM _crap_image_queue WHERE status IN ('completed', 'failed')
             AND completed_at < {}",
            offset_sql
        ),
        &[offset_param],
    )?;
    Ok(deleted as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CrapConfig;
    use crate::db::{BoxedConnection, pool};
    use tempfile::TempDir;

    fn setup_db() -> (TempDir, BoxedConnection) {
        let dir = TempDir::new().unwrap();
        let config = CrapConfig::default();
        let p = pool::create_pool(dir.path(), &config).unwrap();
        let conn = p.get().unwrap();
        conn.execute_batch(
            "CREATE TABLE _crap_image_queue (
                id TEXT PRIMARY KEY,
                collection TEXT NOT NULL,
                document_id TEXT NOT NULL,
                source_path TEXT NOT NULL,
                target_path TEXT NOT NULL,
                format TEXT NOT NULL,
                quality INTEGER NOT NULL,
                url_column TEXT NOT NULL,
                url_value TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'pending',
                error TEXT,
                created_at TEXT DEFAULT (datetime('now')),
                completed_at TEXT
            );",
        )
        .unwrap();
        (dir, conn)
    }

    #[allow(clippy::too_many_arguments)]
    fn entry<'a>(
        collection: &'a str,
        document_id: &'a str,
        source_path: &'a str,
        target_path: &'a str,
        format: &'a str,
        quality: u8,
        url_column: &'a str,
        url_value: &'a str,
    ) -> NewImageEntry<'a> {
        NewImageEntry {
            collection,
            document_id,
            source_path,
            target_path,
            format,
            quality,
            url_column,
            url_value,
        }
    }

    #[test]
    fn insert_and_claim() {
        let (_dir, conn) = setup_db();
        let id = insert_image_queue_entry(
            &conn,
            &entry(
                "media",
                "doc1",
                "/tmp/src.jpg",
                "/tmp/dst.webp",
                "webp",
                80,
                "thumb_webp_url",
                "/uploads/media/thumb.webp",
            ),
        )
        .unwrap();
        assert!(!id.is_empty());

        let entries = claim_pending_images(&conn, 10).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].collection, "media");
        assert_eq!(entries[0].format, "webp");

        // Should not be claimable again (now processing)
        let again = claim_pending_images(&conn, 10).unwrap();
        assert!(again.is_empty());
    }

    #[test]
    fn claim_twice_no_overlap() {
        let (_dir, conn) = setup_db();
        insert_image_queue_entry(&conn, &entry("m", "d1", "/a", "/b", "webp", 80, "c", "u"))
            .unwrap();
        insert_image_queue_entry(&conn, &entry("m", "d2", "/c", "/d", "avif", 60, "c", "u"))
            .unwrap();

        let first = claim_pending_images(&conn, 10).unwrap();
        let second = claim_pending_images(&conn, 10).unwrap();

        // All entries should be claimed by first call, none by second
        assert_eq!(first.len(), 2);
        assert!(second.is_empty(), "second claim should get no entries");
    }

    #[test]
    fn complete_and_fail() {
        let (_dir, conn) = setup_db();
        let id1 = insert_image_queue_entry(
            &conn,
            &entry("media", "d1", "/a", "/b", "webp", 80, "col", "url"),
        )
        .unwrap();
        let id2 = insert_image_queue_entry(
            &conn,
            &entry("media", "d2", "/c", "/d", "avif", 60, "col", "url"),
        )
        .unwrap();

        complete_image_entry(&conn, &id1).unwrap();
        fail_image_entry(&conn, &id2, "decode error").unwrap();

        let row1 = conn
            .query_one(
                "SELECT status FROM _crap_image_queue WHERE id = ?1",
                &[DbValue::Text(id1.clone())],
            )
            .unwrap()
            .unwrap();
        let status1 = if let Some(DbValue::Text(s)) = row1.get_value(0) {
            s.clone()
        } else {
            String::new()
        };
        assert_eq!(status1, "completed");

        let row2 = conn
            .query_one(
                "SELECT status FROM _crap_image_queue WHERE id = ?1",
                &[DbValue::Text(id2.clone())],
            )
            .unwrap()
            .unwrap();
        let status2 = if let Some(DbValue::Text(s)) = row2.get_value(0) {
            s.clone()
        } else {
            String::new()
        };
        assert_eq!(status2, "failed");
    }

    #[test]
    fn count_pending() {
        let (_dir, conn) = setup_db();
        assert_eq!(count_pending_images(&conn).unwrap(), 0);

        insert_image_queue_entry(&conn, &entry("m", "d1", "/a", "/b", "webp", 80, "c", "u"))
            .unwrap();
        insert_image_queue_entry(&conn, &entry("m", "d2", "/c", "/d", "avif", 60, "c", "u"))
            .unwrap();
        assert_eq!(count_pending_images(&conn).unwrap(), 2);

        claim_pending_images(&conn, 1).unwrap();
        assert_eq!(count_pending_images(&conn).unwrap(), 1);
    }

    #[test]
    fn count_by_status() {
        let (_dir, conn) = setup_db();
        assert_eq!(count_image_entries_by_status(&conn, "pending").unwrap(), 0);

        insert_image_queue_entry(&conn, &entry("m", "d1", "/a", "/b", "webp", 80, "c", "u"))
            .unwrap();
        insert_image_queue_entry(&conn, &entry("m", "d2", "/c", "/d", "avif", 60, "c", "u"))
            .unwrap();
        assert_eq!(count_image_entries_by_status(&conn, "pending").unwrap(), 2);

        let claimed = claim_pending_images(&conn, 1).unwrap();
        assert_eq!(claimed.len(), 1);
        assert_eq!(count_image_entries_by_status(&conn, "pending").unwrap(), 1);
        assert_eq!(
            count_image_entries_by_status(&conn, "processing").unwrap(),
            1
        );
    }

    #[test]
    fn list_entries_all() {
        let (_dir, conn) = setup_db();
        insert_image_queue_entry(&conn, &entry("m", "d1", "/a", "/b", "webp", 80, "c", "u"))
            .unwrap();
        insert_image_queue_entry(&conn, &entry("m", "d2", "/c", "/d", "avif", 60, "c", "u"))
            .unwrap();

        let entries = list_image_entries(&conn, None, 100).unwrap();
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn list_entries_filtered() {
        let (_dir, conn) = setup_db();
        let id =
            insert_image_queue_entry(&conn, &entry("m", "d1", "/a", "/b", "webp", 80, "c", "u"))
                .unwrap();
        insert_image_queue_entry(&conn, &entry("m", "d2", "/c", "/d", "avif", 60, "c", "u"))
            .unwrap();

        complete_image_entry(&conn, &id).unwrap();

        let pending = list_image_entries(&conn, Some("pending"), 100).unwrap();
        assert_eq!(pending.len(), 1);

        let completed = list_image_entries(&conn, Some("completed"), 100).unwrap();
        assert_eq!(completed.len(), 1);
    }

    #[test]
    fn retry_single_entry() {
        let (_dir, conn) = setup_db();
        let id =
            insert_image_queue_entry(&conn, &entry("m", "d1", "/a", "/b", "webp", 80, "c", "u"))
                .unwrap();
        fail_image_entry(&conn, &id, "test error").unwrap();

        assert!(retry_image_entry(&conn, &id).unwrap());
        assert_eq!(count_image_entries_by_status(&conn, "pending").unwrap(), 1);
        assert_eq!(count_image_entries_by_status(&conn, "failed").unwrap(), 0);
    }

    #[test]
    fn retry_non_failed_returns_false() {
        let (_dir, conn) = setup_db();
        let id =
            insert_image_queue_entry(&conn, &entry("m", "d1", "/a", "/b", "webp", 80, "c", "u"))
                .unwrap();
        // Still pending, not failed
        assert!(!retry_image_entry(&conn, &id).unwrap());
    }

    #[test]
    fn retry_all_failed() {
        let (_dir, conn) = setup_db();
        let id1 =
            insert_image_queue_entry(&conn, &entry("m", "d1", "/a", "/b", "webp", 80, "c", "u"))
                .unwrap();
        let id2 =
            insert_image_queue_entry(&conn, &entry("m", "d2", "/c", "/d", "avif", 60, "c", "u"))
                .unwrap();
        insert_image_queue_entry(&conn, &entry("m", "d3", "/e", "/f", "webp", 80, "c", "u"))
            .unwrap();

        fail_image_entry(&conn, &id1, "err1").unwrap();
        fail_image_entry(&conn, &id2, "err2").unwrap();

        let count = retry_all_failed_images(&conn).unwrap();
        assert_eq!(count, 2);
        assert_eq!(count_image_entries_by_status(&conn, "pending").unwrap(), 3);
        assert_eq!(count_image_entries_by_status(&conn, "failed").unwrap(), 0);
    }

    #[test]
    fn purge_old_entries() {
        let (_dir, conn) = setup_db();
        let id =
            insert_image_queue_entry(&conn, &entry("m", "d1", "/a", "/b", "webp", 80, "c", "u"))
                .unwrap();
        complete_image_entry(&conn, &id).unwrap();

        // Set completed_at to 2 days ago
        conn.execute(
            "UPDATE _crap_image_queue SET completed_at = datetime('now', '-2 days') WHERE id = ?1",
            &[DbValue::Text(id)],
        )
        .unwrap();

        let purged = purge_old_image_entries(&conn, 86400).unwrap(); // 1 day
        assert_eq!(purged, 1);
    }

    #[test]
    fn recover_stale_processing_entries() {
        let (_dir, conn) = setup_db();

        // Insert two entries and claim them (moves to processing)
        insert_image_queue_entry(
            &conn,
            &entry("m", "d1", "/a", "/b", "avif", 60, "og_avif_url", "/u1"),
        )
        .unwrap();
        insert_image_queue_entry(
            &conn,
            &entry("m", "d1", "/a", "/c", "avif", 60, "hero_avif_url", "/u2"),
        )
        .unwrap();

        // Claim them — both move to processing
        let claimed = claim_pending_images(&conn, 10).unwrap();
        assert_eq!(claimed.len(), 2);

        // Nothing pending anymore
        assert_eq!(count_pending_images(&conn).unwrap(), 0);

        // Recover stale entries — should move both back to pending
        let recovered = recover_stale_images(&conn).unwrap();
        assert_eq!(recovered, 2);

        // Now they're claimable again
        assert_eq!(count_pending_images(&conn).unwrap(), 2);
        let reclaimed = claim_pending_images(&conn, 10).unwrap();
        assert_eq!(reclaimed.len(), 2);
    }

    /// Regression: claim_pending_images must use an atomic UPDATE so that
    /// concurrent callers cannot SELECT the same pending rows before either
    /// marks them as processing (race condition).
    #[test]
    fn claim_is_atomic_no_double_claim() {
        let (_dir, conn) = setup_db();

        // Insert 3 pending entries
        insert_image_queue_entry(&conn, &entry("m", "d1", "/a", "/b", "webp", 80, "c", "u"))
            .unwrap();
        insert_image_queue_entry(&conn, &entry("m", "d2", "/c", "/d", "webp", 80, "c", "u"))
            .unwrap();
        insert_image_queue_entry(&conn, &entry("m", "d3", "/e", "/f", "webp", 80, "c", "u"))
            .unwrap();

        // First caller claims 2
        let first = claim_pending_images(&conn, 2).unwrap();
        assert_eq!(first.len(), 2);

        // Second caller should get only the remaining 1
        let second = claim_pending_images(&conn, 10).unwrap();
        assert_eq!(
            second.len(),
            1,
            "second claim must not re-claim entries from first"
        );

        // No overlap between the two batches
        let first_ids: Vec<_> = first.iter().map(|e| &e.id).collect();
        for e in &second {
            assert!(
                !first_ids.contains(&&e.id),
                "entry {} was double-claimed",
                e.id
            );
        }
    }
}
