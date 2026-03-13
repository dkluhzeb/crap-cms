//! Image processing queue database operations.

use anyhow::{Context as _, Result};

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
    conn: &rusqlite::Connection,
    entry: &NewImageEntry<'_>,
) -> Result<String> {
    let id = nanoid::nanoid!();
    conn.execute(
        "INSERT INTO _crap_image_queue (id, collection, document_id, source_path, target_path, format, quality, url_column, url_value)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        rusqlite::params![id, entry.collection, entry.document_id, entry.source_path, entry.target_path, entry.format, entry.quality, entry.url_column, entry.url_value],
    ).context("Failed to insert image queue entry")?;
    Ok(id)
}

/// Claim up to `limit` pending entries for processing.
pub fn claim_pending_images(
    conn: &rusqlite::Connection,
    limit: usize,
) -> Result<Vec<ImageQueueEntry>> {
    let mut stmt = conn.prepare(
        "SELECT id, collection, document_id, source_path, target_path, format, quality, url_column, url_value
         FROM _crap_image_queue
         WHERE status = 'pending'
         ORDER BY created_at ASC
         LIMIT ?1"
    )?;

    let entries: Vec<ImageQueueEntry> = stmt
        .query_map([limit as i64], |row| {
            Ok(ImageQueueEntry {
                id: row.get(0)?,
                collection: row.get(1)?,
                document_id: row.get(2)?,
                source_path: row.get(3)?,
                target_path: row.get(4)?,
                format: row.get(5)?,
                quality: row.get(6)?,
                url_column: row.get(7)?,
                url_value: row.get(8)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

    // Mark them as processing
    for entry in &entries {
        conn.execute(
            "UPDATE _crap_image_queue SET status = 'processing' WHERE id = ?1",
            [&entry.id],
        )?;
    }

    Ok(entries)
}

/// Mark an entry as completed.
pub fn complete_image_entry(conn: &rusqlite::Connection, id: &str) -> Result<()> {
    conn.execute(
        "UPDATE _crap_image_queue SET status = 'completed', completed_at = datetime('now') WHERE id = ?1",
        [id],
    )?;
    Ok(())
}

/// Mark an entry as failed with an error message.
pub fn fail_image_entry(conn: &rusqlite::Connection, id: &str, error: &str) -> Result<()> {
    conn.execute(
        "UPDATE _crap_image_queue SET status = 'failed', error = ?2, completed_at = datetime('now') WHERE id = ?1",
        rusqlite::params![id, error],
    )?;
    Ok(())
}

/// Count pending entries in the queue.
pub fn count_pending_images(conn: &rusqlite::Connection) -> Result<i64> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM _crap_image_queue WHERE status = 'pending'",
        [],
        |row| row.get(0),
    )?;
    Ok(count)
}

/// Count entries by status.
pub fn count_image_entries_by_status(conn: &rusqlite::Connection, status: &str) -> Result<i64> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM _crap_image_queue WHERE status = ?1",
        [status],
        |row| row.get(0),
    )?;
    Ok(count)
}

/// List queue entries with optional status filter and limit.
pub fn list_image_entries(
    conn: &rusqlite::Connection,
    status_filter: Option<&str>,
    limit: i64,
) -> Result<Vec<ImageQueueListEntry>> {
    let (sql, params): (String, Vec<Box<dyn rusqlite::types::ToSql>>) = match status_filter {
        Some(status) => (
            "SELECT id, collection, document_id, format, status, error, created_at, completed_at \
             FROM _crap_image_queue WHERE status = ?1 ORDER BY created_at DESC LIMIT ?2"
                .to_string(),
            vec![
                Box::new(status.to_string()) as Box<dyn rusqlite::types::ToSql>,
                Box::new(limit),
            ],
        ),
        None => (
            "SELECT id, collection, document_id, format, status, error, created_at, completed_at \
             FROM _crap_image_queue ORDER BY created_at DESC LIMIT ?1"
                .to_string(),
            vec![Box::new(limit) as Box<dyn rusqlite::types::ToSql>],
        ),
    };

    let mut stmt = conn.prepare(&sql)?;
    let entries = stmt
        .query_map(rusqlite::params_from_iter(params), |row| {
            Ok(ImageQueueListEntry {
                id: row.get(0)?,
                collection: row.get(1)?,
                document_id: row.get(2)?,
                format: row.get(3)?,
                status: row.get(4)?,
                error: row.get(5)?,
                created_at: row.get(6)?,
                completed_at: row.get(7)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

    Ok(entries)
}

/// Reset a single failed entry to pending for retry.
pub fn retry_image_entry(conn: &rusqlite::Connection, id: &str) -> Result<bool> {
    let updated = conn.execute(
        "UPDATE _crap_image_queue SET status = 'pending', error = NULL, completed_at = NULL \
         WHERE id = ?1 AND status = 'failed'",
        [id],
    )?;
    Ok(updated > 0)
}

/// Reset all failed entries to pending. Returns the count of reset entries.
pub fn retry_all_failed_images(conn: &rusqlite::Connection) -> Result<i64> {
    let updated = conn.execute(
        "UPDATE _crap_image_queue SET status = 'pending', error = NULL, completed_at = NULL \
         WHERE status = 'failed'",
        [],
    )?;
    Ok(updated as i64)
}

/// Purge completed/failed entries older than the given number of seconds.
pub fn purge_old_image_entries(conn: &rusqlite::Connection, older_than_secs: u64) -> Result<i64> {
    let deleted = conn.execute(
        "DELETE FROM _crap_image_queue WHERE status IN ('completed', 'failed')
         AND completed_at < datetime('now', '-' || ?1 || ' seconds')",
        [older_than_secs.to_string()],
    )?;
    Ok(deleted as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_db() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
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
        conn
    }

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
        let conn = setup_db();
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
    fn complete_and_fail() {
        let conn = setup_db();
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

        let status1: String = conn
            .query_row(
                "SELECT status FROM _crap_image_queue WHERE id = ?1",
                [&id1],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status1, "completed");

        let status2: String = conn
            .query_row(
                "SELECT status FROM _crap_image_queue WHERE id = ?1",
                [&id2],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status2, "failed");
    }

    #[test]
    fn count_pending() {
        let conn = setup_db();
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
        let conn = setup_db();
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
        let conn = setup_db();
        insert_image_queue_entry(&conn, &entry("m", "d1", "/a", "/b", "webp", 80, "c", "u"))
            .unwrap();
        insert_image_queue_entry(&conn, &entry("m", "d2", "/c", "/d", "avif", 60, "c", "u"))
            .unwrap();

        let entries = list_image_entries(&conn, None, 100).unwrap();
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn list_entries_filtered() {
        let conn = setup_db();
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
        let conn = setup_db();
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
        let conn = setup_db();
        let id =
            insert_image_queue_entry(&conn, &entry("m", "d1", "/a", "/b", "webp", 80, "c", "u"))
                .unwrap();
        // Still pending, not failed
        assert!(!retry_image_entry(&conn, &id).unwrap());
    }

    #[test]
    fn retry_all_failed() {
        let conn = setup_db();
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
        let conn = setup_db();
        let id =
            insert_image_queue_entry(&conn, &entry("m", "d1", "/a", "/b", "webp", 80, "c", "u"))
                .unwrap();
        complete_image_entry(&conn, &id).unwrap();

        // Set completed_at to 2 days ago
        conn.execute(
            "UPDATE _crap_image_queue SET completed_at = datetime('now', '-2 days') WHERE id = ?1",
            [&id],
        )
        .unwrap();

        let purged = purge_old_image_entries(&conn, 86400).unwrap(); // 1 day
        assert_eq!(purged, 1);
    }
}
