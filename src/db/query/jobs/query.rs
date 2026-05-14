//! Read-only queries: counts, lists, single-row fetches.

use std::collections::HashMap;
use std::fmt::Write as _;

use anyhow::{Context as _, Result};

use crate::core::{JobRun, JobStatus};
use crate::db::{DbConnection, DbRow, DbValue};

/// Count running jobs, optionally filtered by slug.
///
/// # Errors
///
/// Returns a backend error if the COUNT query fails.
pub fn count_running(conn: &dyn DbConnection, slug: Option<&str>) -> Result<i64> {
    let row = match slug {
        Some(s) => conn.query_one(
            &format!(
                "SELECT COUNT(*) FROM _crap_jobs WHERE status = 'running' AND slug = {}",
                conn.placeholder(1)
            ),
            &[DbValue::Text(s.to_string())],
        )?,
        None => conn.query_one(
            "SELECT COUNT(*) FROM _crap_jobs WHERE status = 'running'",
            &[],
        )?,
    };

    Ok(row.as_ref().and_then(|r| r.i64_at(0)).unwrap_or(0))
}

/// Count running jobs per slug, returned as a `HashMap`.
///
/// # Errors
///
/// Returns a backend error if the COUNT query fails.
pub fn count_running_per_slug(conn: &dyn DbConnection) -> Result<HashMap<String, i64>> {
    let rows = conn.query_all(
        "SELECT slug, COUNT(*) FROM _crap_jobs WHERE status = 'running' GROUP BY slug",
        &[],
    )?;
    let mut map = HashMap::new();

    for row in rows {
        let Some(slug) = row.opt_text_at(0) else {
            continue;
        };
        let count = row.i64_at(1).unwrap_or(0);

        map.insert(slug, count);
    }

    Ok(map)
}

/// Count job runs with optional filters (same WHERE clause as [`list_job_runs`]).
///
/// # Errors
///
/// Returns a backend error if the COUNT query fails.
pub fn count_job_runs(
    conn: &dyn DbConnection,
    slug: Option<&str>,
    status: Option<&str>,
) -> Result<i64> {
    let mut sql = String::from("SELECT COUNT(*) FROM _crap_jobs WHERE 1=1");
    let mut params: Vec<DbValue> = Vec::new();

    if let Some(s) = slug {
        params.push(DbValue::Text(s.to_string()));
        let _ = write!(sql, " AND slug = {}", conn.placeholder(params.len()));
    }

    if let Some(st) = status {
        params.push(DbValue::Text(st.to_string()));
        let _ = write!(sql, " AND status = {}", conn.placeholder(params.len()));
    }

    let row = conn.query_one(&sql, &params)?;

    Ok(row.as_ref().and_then(|r| r.i64_at(0)).unwrap_or(0))
}

/// List job runs with optional filters.
///
/// # Errors
///
/// Returns a backend error if the SELECT query fails or any row fails to parse.
pub fn list_job_runs(
    conn: &dyn DbConnection,
    slug: Option<&str>,
    status: Option<&str>,
    limit: i64,
    offset: i64,
) -> Result<Vec<JobRun>> {
    let mut sql = String::from(
        "SELECT id, slug, status, queue, data, result, error, attempt, max_attempts,
                scheduled_by, created_at, started_at, completed_at, heartbeat_at, retry_after
         FROM _crap_jobs WHERE 1=1",
    );
    let mut params: Vec<DbValue> = Vec::new();

    if let Some(s) = slug {
        params.push(DbValue::Text(s.to_string()));
        let _ = write!(sql, " AND slug = {}", conn.placeholder(params.len()));
    }
    if let Some(st) = status {
        params.push(DbValue::Text(st.to_string()));
        let _ = write!(sql, " AND status = {}", conn.placeholder(params.len()));
    }

    params.push(DbValue::Integer(limit));
    let _ = write!(
        sql,
        " ORDER BY created_at DESC LIMIT {}",
        conn.placeholder(params.len())
    );

    params.push(DbValue::Integer(offset));
    let _ = write!(sql, " OFFSET {}", conn.placeholder(params.len()));

    let rows = conn.query_all(&sql, &params)?;

    Ok(rows.iter().map(row_to_job_run).collect())
}

/// Get a single job run by ID.
///
/// # Errors
///
/// Returns a backend error if the SELECT query fails or the row fails to parse.
pub fn get_job_run(conn: &dyn DbConnection, id: &str) -> Result<Option<JobRun>> {
    let row = conn.query_one(
        &format!(
            "SELECT id, slug, status, queue, data, result, error, attempt, max_attempts,
                scheduled_by, created_at, started_at, completed_at, heartbeat_at, retry_after
         FROM _crap_jobs WHERE id = {}",
            conn.placeholder(1)
        ),
        &[DbValue::Text(id.to_string())],
    )?;

    Ok(row.map(|r| row_to_job_run(&r)))
}

/// Find jobs that are marked as running but have a stale heartbeat.
///
/// # Errors
///
/// Returns a backend error if the SELECT query fails or any row fails to parse.
pub fn find_stale_jobs(conn: &dyn DbConnection, stale_threshold_secs: u64) -> Result<Vec<JobRun>> {
    let threshold = i64::try_from(stale_threshold_secs)
        .context("stale_threshold_secs exceeds the SQL TIMESTAMP arithmetic range")?;
    let (offset_sql, offset_param) = conn.date_offset_expr(threshold, 1);
    let rows = conn.query_all(
        &format!(
            "SELECT id, slug, status, queue, data, result, error, attempt, max_attempts,
                    scheduled_by, created_at, started_at, completed_at, heartbeat_at, retry_after
             FROM _crap_jobs
             WHERE status = 'running'
               AND (heartbeat_at IS NULL OR heartbeat_at < {offset_sql})"
        ),
        &[offset_param],
    )?;

    Ok(rows.iter().map(row_to_job_run).collect())
}

/// Count failed jobs within a recent time window (in seconds).
///
/// # Errors
///
/// Returns a backend error if the COUNT query fails.
pub fn count_failed_since(conn: &dyn DbConnection, since_secs: u64) -> Result<i64> {
    let since = i64::try_from(since_secs)
        .context("since_secs exceeds the SQL TIMESTAMP arithmetic range")?;
    let (offset_sql, offset_param) = conn.date_offset_expr(since, 1);
    let row = conn.query_one(
        &format!(
            "SELECT COUNT(*) FROM _crap_jobs
             WHERE status = 'failed' AND completed_at >= {offset_sql}"
        ),
        &[offset_param],
    )?;

    Ok(row.as_ref().and_then(|r| r.i64_at(0)).unwrap_or(0))
}

/// Count pending jobs that have been waiting longer than the given threshold (in seconds).
///
/// # Errors
///
/// Returns a backend error if the COUNT query fails.
pub fn count_pending_older_than(conn: &dyn DbConnection, older_than_secs: u64) -> Result<i64> {
    let older = i64::try_from(older_than_secs)
        .context("older_than_secs exceeds the SQL TIMESTAMP arithmetic range")?;
    let (offset_sql, offset_param) = conn.date_offset_expr(older, 1);
    let row = conn.query_one(
        &format!(
            "SELECT COUNT(*) FROM _crap_jobs
             WHERE status = 'pending' AND created_at < {offset_sql}"
        ),
        &[offset_param],
    )?;

    Ok(row.as_ref().and_then(|r| r.i64_at(0)).unwrap_or(0))
}

/// Get the most recent completed run for a given job slug.
///
/// # Errors
///
/// Returns a backend error if the SELECT query fails or the row fails to parse.
pub fn last_completed_run(conn: &dyn DbConnection, slug: &str) -> Result<Option<JobRun>> {
    let row = conn.query_one(
        &format!(
            "SELECT id, slug, status, queue, data, result, error, attempt, max_attempts,
                scheduled_by, created_at, started_at, completed_at, heartbeat_at, retry_after
         FROM _crap_jobs
         WHERE slug = {} AND status = 'completed'
         ORDER BY completed_at DESC
         LIMIT 1",
            conn.placeholder(1)
        ),
        &[DbValue::Text(slug.to_string())],
    )?;

    Ok(row.map(|r| row_to_job_run(&r)))
}

/// Build a `JobRun` from a wide DB row that includes all 15 columns.
///
/// All column accessors fall back to sensible defaults (empty string,
/// `JobStatus::Pending`, `0` for counters), so this never fails — a
/// malformed row produces a logically-empty `JobRun` rather than an error.
fn row_to_job_run(row: &DbRow) -> JobRun {
    let text_or =
        |idx: usize, default: &str| -> String { row.text_at(idx).unwrap_or(default).to_string() };

    let id = text_or(0, "");
    let slug = text_or(1, "");
    let status =
        JobStatus::from_name(row.text_at(2).unwrap_or("pending")).unwrap_or(JobStatus::Pending);

    let mut b = JobRun::builder(id, slug)
        .status(status)
        .queue(text_or(3, "default"))
        .data(text_or(4, "{}"))
        .attempt(
            row.i64_at(7)
                .and_then(|v| u32::try_from(v).ok())
                .unwrap_or(0),
        )
        .max_attempts(
            row.i64_at(8)
                .and_then(|v| u32::try_from(v).ok())
                .unwrap_or(0),
        );

    if let Some(r) = row.opt_text_at(5) {
        b = b.result(r);
    }

    if let Some(e) = row.opt_text_at(6) {
        b = b.error(e);
    }

    if let Some(sb) = row.opt_text_at(9) {
        b = b.scheduled_by(sb);
    }

    if let Some(ca) = row.opt_text_at(10) {
        b = b.created_at(ca);
    }

    if let Some(sa) = row.opt_text_at(11) {
        b = b.started_at(sa);
    }

    if let Some(ca) = row.opt_text_at(12) {
        b = b.completed_at(ca);
    }

    if let Some(ha) = row.opt_text_at(13) {
        b = b.heartbeat_at(ha);
    }

    if let Some(ra) = row.opt_text_at(14) {
        b = b.retry_after(ra);
    }

    b.build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::query::jobs::insert_job;
    use crate::db::query::jobs::test_helpers::setup_db;

    #[test]
    fn test_count_running() {
        let (_dir, conn) = setup_db();
        insert_job(&conn, "job_a", "{}", "cron", 1, "default").unwrap();
        insert_job(&conn, "job_b", "{}", "cron", 1, "default").unwrap();
        conn.execute(
            "UPDATE _crap_jobs SET status = 'running' WHERE slug = 'job_a'",
            &[],
        )
        .unwrap();

        assert_eq!(count_running(&conn, None).unwrap(), 1);
        assert_eq!(count_running(&conn, Some("job_a")).unwrap(), 1);
        assert_eq!(count_running(&conn, Some("job_b")).unwrap(), 0);
    }

    #[test]
    fn test_list_job_runs() {
        let (_dir, conn) = setup_db();
        insert_job(&conn, "job_a", "{}", "cron", 1, "default").unwrap();
        insert_job(&conn, "job_b", "{}", "manual", 1, "default").unwrap();

        let all = list_job_runs(&conn, None, None, 100, 0).unwrap();
        assert_eq!(all.len(), 2);

        let filtered = list_job_runs(&conn, Some("job_a"), None, 100, 0).unwrap();
        assert_eq!(filtered.len(), 1);
    }

    #[test]
    fn test_count_running_per_slug() {
        let (_dir, conn) = setup_db();
        insert_job(&conn, "job_a", "{}", "cron", 1, "default").unwrap();
        insert_job(&conn, "job_a", "{}", "cron", 1, "default").unwrap();
        insert_job(&conn, "job_b", "{}", "cron", 1, "default").unwrap();

        conn.execute(
            "UPDATE _crap_jobs SET status = 'running' WHERE slug = 'job_a'",
            &[],
        )
        .unwrap();
        conn.execute(
            "UPDATE _crap_jobs SET status = 'running' WHERE slug = 'job_b'",
            &[],
        )
        .unwrap();

        let counts = count_running_per_slug(&conn).unwrap();
        assert_eq!(counts.get("job_a").copied(), Some(2));
        assert_eq!(counts.get("job_b").copied(), Some(1));
    }

    #[test]
    fn test_get_job_run_not_found() {
        let (_dir, conn) = setup_db();
        let result = get_job_run(&conn, "nonexistent").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_list_job_runs_with_status_filter() {
        let (_dir, conn) = setup_db();
        insert_job(&conn, "job_a", "{}", "cron", 1, "default").unwrap();
        insert_job(&conn, "job_b", "{}", "cron", 1, "default").unwrap();
        conn.execute(
            "UPDATE _crap_jobs SET status = 'running' WHERE slug = 'job_a'",
            &[],
        )
        .unwrap();

        let running = list_job_runs(&conn, None, Some("running"), 100, 0).unwrap();
        assert_eq!(running.len(), 1);
        assert_eq!(running[0].slug, "job_a");

        let pending = list_job_runs(&conn, None, Some("pending"), 100, 0).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].slug, "job_b");
    }

    #[test]
    fn test_find_stale_jobs() {
        let (_dir, conn) = setup_db();
        let job = insert_job(&conn, "test", "{}", "manual", 1, "default").unwrap();
        // Set job as running with a stale heartbeat
        conn.execute(
            "UPDATE _crap_jobs SET status = 'running', heartbeat_at = datetime('now', '-3600 seconds') WHERE id = ?1",
            &[DbValue::Text(job.id.clone())],
        ).unwrap();

        let stale = find_stale_jobs(&conn, 60).unwrap();
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].id, job.id);

        // With a very long threshold, nothing should be stale
        let stale = find_stale_jobs(&conn, 99999).unwrap();
        assert!(stale.is_empty());
    }

    #[test]
    fn test_count_failed_since() {
        let (_dir, conn) = setup_db();
        // Insert a recently failed job
        conn.execute(
            "INSERT INTO _crap_jobs (id, slug, status, completed_at) VALUES ('f1', 'test', 'failed', datetime('now'))",
            &[],
        ).unwrap();
        // Insert an old failed job
        conn.execute(
            "INSERT INTO _crap_jobs (id, slug, status, completed_at) VALUES ('f2', 'test', 'failed', datetime('now', '-48 hours'))",
            &[],
        ).unwrap();
        // Insert a completed job (should not count)
        conn.execute(
            "INSERT INTO _crap_jobs (id, slug, status, completed_at) VALUES ('c1', 'test', 'completed', datetime('now'))",
            &[],
        ).unwrap();

        let count = count_failed_since(&conn, 86400).unwrap(); // 24h
        assert_eq!(count, 1, "only the recent failure should count");

        let count_all = count_failed_since(&conn, 86400 * 3).unwrap(); // 3 days
        assert_eq!(count_all, 2, "both failures within 3 days");
    }

    #[test]
    fn test_count_pending_older_than() {
        let (_dir, conn) = setup_db();
        // Insert a pending job from 10 minutes ago
        conn.execute(
            "INSERT INTO _crap_jobs (id, slug, status, created_at) VALUES ('p1', 'test', 'pending', datetime('now', '-600 seconds'))",
            &[],
        ).unwrap();
        // Insert a recent pending job
        conn.execute(
            "INSERT INTO _crap_jobs (id, slug, status, created_at) VALUES ('p2', 'test', 'pending', datetime('now'))",
            &[],
        ).unwrap();

        let count = count_pending_older_than(&conn, 300).unwrap(); // 5 min
        assert_eq!(count, 1, "only the old pending job should count");

        let count_all = count_pending_older_than(&conn, 1).unwrap(); // 1 second
        assert_eq!(count_all, 1, "still just the old one");
    }

    #[test]
    fn test_last_completed_run() {
        let (_dir, conn) = setup_db();

        // No completed runs
        let last = last_completed_run(&conn, "test").unwrap();
        assert!(last.is_none());

        // Add a completed run
        conn.execute(
            "INSERT INTO _crap_jobs (id, slug, status, completed_at) VALUES ('c1', 'test', 'completed', datetime('now', '-1 hour'))",
            &[],
        ).unwrap();
        conn.execute(
            "INSERT INTO _crap_jobs (id, slug, status, completed_at) VALUES ('c2', 'test', 'completed', datetime('now'))",
            &[],
        ).unwrap();

        let last = last_completed_run(&conn, "test").unwrap().unwrap();
        assert_eq!(last.id, "c2", "should return the most recent completed run");

        // Different slug should return None
        let other = last_completed_run(&conn, "other").unwrap();
        assert!(other.is_none());
    }
}
