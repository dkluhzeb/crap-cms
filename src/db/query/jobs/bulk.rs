//! Bulk delete operations: cancel pending, purge old.

use anyhow::Result;

use crate::db::{DbConnection, DbValue};

/// Cancel pending jobs. Optionally filter by job slug.
pub fn cancel_pending_jobs(conn: &dyn DbConnection, slug: Option<&str>) -> Result<i64> {
    let deleted = if let Some(slug) = slug {
        conn.execute(
            &format!(
                "DELETE FROM _crap_jobs WHERE status = 'pending' AND slug = {}",
                conn.placeholder(1)
            ),
            &[DbValue::Text(slug.to_string())],
        )? as i64
    } else {
        conn.execute("DELETE FROM _crap_jobs WHERE status = 'pending'", &[])? as i64
    };

    Ok(deleted)
}

/// Delete completed/failed/stale job runs older than the given threshold.
/// Returns the number of rows deleted.
pub fn purge_old_jobs(conn: &dyn DbConnection, older_than_secs: u64) -> Result<i64> {
    let (offset_sql, offset_param) = conn.date_offset_expr(older_than_secs as i64, 1);
    let deleted = conn.execute(
        &format!(
            "DELETE FROM _crap_jobs
             WHERE status IN ('completed', 'failed', 'stale')
               AND created_at < {offset_sql}"
        ),
        &[offset_param],
    )? as i64;

    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::job::JobStatus;
    use crate::db::query::jobs::test_helpers::setup_db;
    use crate::db::query::jobs::{insert_job, list_job_runs};

    #[test]
    fn test_purge_old_jobs() {
        let (_dir, conn) = setup_db();
        // Insert a completed job with old timestamp
        conn.execute(
            "INSERT INTO _crap_jobs (id, slug, status, created_at) VALUES ('old1', 'test', 'completed', datetime('now', '-30 days'))",
            &[],
        ).unwrap();
        conn.execute(
            "INSERT INTO _crap_jobs (id, slug, status, created_at) VALUES ('new1', 'test', 'completed', datetime('now'))",
            &[],
        ).unwrap();

        let deleted = purge_old_jobs(&conn, 86400 * 7).unwrap(); // 7 days
        assert_eq!(deleted, 1);

        let remaining = list_job_runs(&conn, None, None, 100, 0).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, "new1");
    }

    /// Regression: cancel_pending_jobs used `name` instead of `slug` column.
    #[test]
    fn test_cancel_pending_jobs_by_slug() {
        let (_dir, conn) = setup_db();

        insert_job(&conn, "cleanup", "{}", "cli", 1, "default").unwrap();
        insert_job(&conn, "notify", "{}", "cli", 1, "default").unwrap();

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
}
