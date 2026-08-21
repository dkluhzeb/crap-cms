//! Bulk delete operations: cancel pending, purge old.

use std::fmt::Write as _;

use anyhow::{Context as _, Result};

use crate::db::{DbConnection, DbValue};

/// Cancel pending jobs. Optionally filter by job slug.
///
/// # Errors
///
/// Returns a backend error if the DELETE fails.
pub fn cancel_pending_jobs(conn: &dyn DbConnection, slug: Option<&str>) -> Result<i64> {
    let affected = if let Some(slug) = slug {
        conn.execute(
            &format!(
                "DELETE FROM _crap_jobs WHERE status = 'pending' AND slug = {}",
                conn.placeholder(1)
            ),
            &[DbValue::Text(slug.to_string())],
        )?
    } else {
        conn.execute("DELETE FROM _crap_jobs WHERE status = 'pending'", &[])?
    };

    i64::try_from(affected).context("cancelled count exceeds i64::MAX")
}

/// Delete completed/failed/stale job runs older than the given threshold.
/// Returns the number of rows deleted.
///
/// # Errors
///
/// Returns a backend error if the DELETE fails.
pub fn purge_old_jobs(conn: &dyn DbConnection, older_than_secs: u64) -> Result<i64> {
    let older = i64::try_from(older_than_secs)
        .context("older_than_secs exceeds the SQL TIMESTAMP arithmetic range")?;
    let (offset_sql, offset_param) = conn.date_offset_expr(older, 1);
    let deleted = i64::try_from(conn.execute(
        &format!(
            "DELETE FROM _crap_jobs
             WHERE status IN ('completed', 'failed', 'stale')
               AND created_at < {offset_sql}"
        ),
        &[offset_param],
    )?)
    .context("delete count exceeds i64::MAX")?;

    Ok(deleted)
}

/// Delete pending/failed jobs of `slug` whose `data` matches ALL of the given
/// `LIKE` patterns. Used to drop superseded image-convert jobs when their
/// owning document is deleted. Patterns are bound as parameters.
///
/// # Errors
///
/// Returns a backend error if the DELETE fails.
pub fn delete_pending_failed_jobs_matching(
    conn: &dyn DbConnection,
    slug: &str,
    data_patterns: &[String],
) -> Result<()> {
    let mut sql = format!(
        "DELETE FROM _crap_jobs WHERE slug = {} AND status IN ('pending', 'failed')",
        conn.placeholder(1)
    );
    let mut params: Vec<DbValue> = vec![DbValue::Text(slug.to_string())];

    for pat in data_patterns {
        params.push(DbValue::Text(pat.clone()));
        let _ = write!(sql, " AND data LIKE {}", conn.placeholder(params.len()));
    }

    conn.execute(&sql, &params)
        .with_context(|| format!("Failed to delete pending/failed jobs for slug '{slug}'"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::JobStatus;
    use crate::db::query::jobs::test_helpers::setup_db;
    use crate::db::query::jobs::{insert_job, list_job_runs};

    #[test]
    fn test_purge_old_jobs() {
        let (_dir, conn) = setup_db();
        // Insert a completed job with old timestamp
        conn.execute(
            "INSERT INTO _crap_jobs (id, slug, status, created_at) VALUES ('old1', 'test', 'completed', strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-30 days'))",
            &[],
        ).unwrap();
        conn.execute(
            "INSERT INTO _crap_jobs (id, slug, status, created_at) VALUES ('new1', 'test', 'completed', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
            &[],
        ).unwrap();

        let deleted = purge_old_jobs(&conn, 86400 * 7).unwrap(); // 7 days
        assert_eq!(deleted, 1);

        let remaining = list_job_runs(&conn, None, None, 100, 0).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, "new1");
    }

    /// Regression: `cancel_pending_jobs` used `name` instead of `slug` column.
    #[test]
    fn test_cancel_pending_jobs_by_slug() {
        let (_dir, conn) = setup_db();

        insert_job(&conn, "cleanup", "{}", "cli", 1, "default", 0).unwrap();
        insert_job(&conn, "notify", "{}", "cli", 1, "default", 0).unwrap();

        // Cancel only "cleanup" pending jobs
        let deleted = cancel_pending_jobs(&conn, Some("cleanup")).unwrap();
        assert_eq!(deleted, 1, "should cancel exactly one job");

        // "notify" should still be pending
        let runs = list_job_runs(&conn, Some("notify"), None, 10, 0).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, JobStatus::Pending);

        // Cancel all remaining pending
        let deleted = cancel_pending_jobs(&conn, None).unwrap();
        assert_eq!(deleted, 1, "should cancel the remaining pending job");
    }

    #[test]
    fn delete_pending_failed_jobs_matching_filters_by_slug_and_data() {
        let (_dir, conn) = setup_db();

        insert_job(
            &conn,
            "img",
            r#"{"collection":"media","document_id":"d1"}"#,
            "sys",
            1,
            "default",
            0,
        )
        .unwrap();
        insert_job(
            &conn,
            "img",
            r#"{"collection":"media","document_id":"d2"}"#,
            "sys",
            1,
            "default",
            0,
        )
        .unwrap();
        // Same payload but a different slug — must be left alone.
        insert_job(
            &conn,
            "other",
            r#"{"collection":"media","document_id":"d1"}"#,
            "sys",
            1,
            "default",
            0,
        )
        .unwrap();

        let patterns = vec![
            "%\"collection\":\"media\"%".to_string(),
            "%\"document_id\":\"d1\"%".to_string(),
        ];
        delete_pending_failed_jobs_matching(&conn, "img", &patterns).unwrap();

        let remaining = list_job_runs(&conn, None, None, 100, 0).unwrap();
        assert_eq!(remaining.len(), 2, "only img+d1 should be deleted");
        assert!(
            !remaining
                .iter()
                .any(|r| r.slug == "img" && r.data.contains("\"d1\"")),
            "the matching img/d1 job must be gone"
        );
        assert!(
            remaining
                .iter()
                .any(|r| r.slug == "img" && r.data.contains("\"d2\"")),
            "a non-matching data pattern (d2) must be kept"
        );
        assert!(
            remaining.iter().any(|r| r.slug == "other"),
            "a different slug must be kept even with matching data"
        );
    }
}
