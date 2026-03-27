//! CRUD query functions for the `_crap_jobs` table.

use crate::core::job::{JobRun, JobStatus};
use crate::db::{DbConnection, DbRow, DbValue};
use anyhow::{Context as _, Result};

/// Insert a new pending job run.
pub fn insert_job(
    conn: &dyn DbConnection,
    slug: &str,
    data: &str,
    scheduled_by: &str,
    max_attempts: u32,
    queue: &str,
) -> Result<JobRun> {
    let id = nanoid::nanoid!();
    let (p1, p2, p3, p4, p5, p6) = (
        conn.placeholder(1),
        conn.placeholder(2),
        conn.placeholder(3),
        conn.placeholder(4),
        conn.placeholder(5),
        conn.placeholder(6),
    );
    conn.execute(
        &format!(
            "INSERT INTO _crap_jobs (id, slug, status, queue, data, max_attempts, scheduled_by)
         VALUES ({p1}, {p2}, 'pending', {p3}, {p4}, {p5}, {p6})"
        ),
        &[
            DbValue::Text(id.clone()),
            DbValue::Text(slug.to_string()),
            DbValue::Text(queue.to_string()),
            DbValue::Text(data.to_string()),
            DbValue::Integer(max_attempts as i64),
            DbValue::Text(scheduled_by.to_string()),
        ],
    )
    .context("Failed to insert job run")?;

    Ok(JobRun::builder(id, slug)
        .queue(queue)
        .data(data)
        .max_attempts(max_attempts)
        .scheduled_by(scheduled_by)
        .build())
}

/// Atomically claim up to `limit` pending jobs by setting them to running.
/// Returns the claimed jobs. Respects per-job concurrency limits.
pub fn claim_pending_jobs(
    conn: &dyn DbConnection,
    limit: usize,
    running_counts: &std::collections::HashMap<String, i64>,
    job_concurrency: &std::collections::HashMap<String, u32>,
) -> Result<Vec<JobRun>> {
    let now = conn.now_expr();
    let rows = conn.query_all(
        &format!(
            "SELECT id, slug, queue, data, attempt, max_attempts, scheduled_by, created_at
         FROM _crap_jobs
         WHERE status = 'pending'
           AND (retry_after IS NULL OR retry_after <= {now})
         ORDER BY created_at ASC
         LIMIT {}",
            conn.placeholder(1)
        ),
        &[DbValue::Integer((limit * 2) as i64)],
    )?;

    let mut claimed = Vec::new();
    let mut extra_running: std::collections::HashMap<String, i64> =
        std::collections::HashMap::new();

    for row in rows {
        if claimed.len() >= limit {
            break;
        }

        let id = match row.get_value(0) {
            Some(DbValue::Text(s)) => s.clone(),
            _ => continue,
        };
        let slug = match row.get_value(1) {
            Some(DbValue::Text(s)) => s.clone(),
            _ => continue,
        };
        let queue = match row.get_value(2) {
            Some(DbValue::Text(s)) => s.clone(),
            _ => "default".to_string(),
        };
        let data = match row.get_value(3) {
            Some(DbValue::Text(s)) => s.clone(),
            _ => "{}".to_string(),
        };
        let attempt = match row.get_value(4) {
            Some(DbValue::Integer(n)) => *n as u32,
            _ => 0,
        };
        let max_attempts = match row.get_value(5) {
            Some(DbValue::Integer(n)) => *n as u32,
            _ => 1,
        };
        let scheduled_by: Option<String> = match row.get_value(6) {
            Some(DbValue::Text(s)) => Some(s.clone()),
            _ => None,
        };
        let created_at: Option<String> = match row.get_value(7) {
            Some(DbValue::Text(s)) => Some(s.clone()),
            _ => None,
        };

        // Check per-job concurrency
        let max_conc = job_concurrency.get(&slug).copied().unwrap_or(1) as i64;
        let current = running_counts.get(&slug).copied().unwrap_or(0)
            + extra_running.get(&slug).copied().unwrap_or(0);

        if current >= max_conc {
            continue;
        }

        // Claim the job
        let now = conn.now_expr();
        let p1 = conn.placeholder(1);
        let affected = conn.execute(
            &format!("UPDATE _crap_jobs SET status = 'running', started_at = {now}, heartbeat_at = {now}, attempt = attempt + 1
             WHERE id = {p1} AND status = 'pending'"),
            &[DbValue::Text(id.clone())],
        )?;

        if affected > 0 {
            *extra_running.entry(slug.clone()).or_insert(0) += 1;
            let mut b = JobRun::builder(id, slug)
                .status(JobStatus::Running)
                .queue(queue)
                .data(data)
                .attempt(attempt + 1)
                .max_attempts(max_attempts);

            if let Some(sb) = scheduled_by {
                b = b.scheduled_by(sb);
            }
            if let Some(ca) = created_at {
                b = b.created_at(ca);
            }
            claimed.push(b.build());
        }
    }

    Ok(claimed)
}

/// Mark a job as completed with an optional result.
pub fn complete_job(conn: &dyn DbConnection, id: &str, result_json: Option<&str>) -> Result<()> {
    let result_val = match result_json {
        Some(r) => DbValue::Text(r.to_string()),
        None => DbValue::Null,
    };
    let (p1, p2) = (conn.placeholder(1), conn.placeholder(2));
    conn.execute(
        &format!(
            "UPDATE _crap_jobs SET status = 'completed', result = {p2}, completed_at = {}
         WHERE id = {p1}",
            conn.now_expr()
        ),
        &[DbValue::Text(id.to_string()), result_val],
    )
    .context("Failed to complete job")?;
    Ok(())
}

/// Compute exponential backoff delay in seconds for a given attempt number.
///
/// Formula: `min(2^attempt * 5, 300)` — yields 5s, 10s, 20s, 40s, 80s, 160s, 300s cap.
fn backoff_seconds(attempt: u32) -> i64 {
    std::cmp::min(5 * (1i64 << attempt.min(6) as i64), 300)
}

/// Mark a job as failed. If should_retry is true, resets to pending with exponential backoff.
/// `attempt` is the current attempt number (already incremented by claim).
pub fn fail_job(
    conn: &dyn DbConnection,
    id: &str,
    error: &str,
    should_retry: bool,
    attempt: u32,
) -> Result<()> {
    let (p1, p2) = (conn.placeholder(1), conn.placeholder(2));

    if should_retry {
        let delay = backoff_seconds(attempt);
        let retry_after_expr = format!("datetime('now', '+{} seconds')", delay);

        conn.execute(
            &format!(
                "UPDATE _crap_jobs SET status = 'pending', error = {p2}, \
                 started_at = NULL, completed_at = NULL, heartbeat_at = NULL, \
                 retry_after = {retry_after_expr} \
                 WHERE id = {p1}"
            ),
            &[
                DbValue::Text(id.to_string()),
                DbValue::Text(error.to_string()),
            ],
        )
        .context("Failed to retry job")?;
    } else {
        conn.execute(
            &format!(
                "UPDATE _crap_jobs SET status = 'failed', error = {p2}, completed_at = {}
             WHERE id = {p1}",
                conn.now_expr()
            ),
            &[
                DbValue::Text(id.to_string()),
                DbValue::Text(error.to_string()),
            ],
        )
        .context("Failed to fail job")?;
    }

    Ok(())
}

/// Update the heartbeat timestamp for a running job.
pub fn update_heartbeat(conn: &dyn DbConnection, id: &str) -> Result<()> {
    conn.execute(
        &format!(
            "UPDATE _crap_jobs SET heartbeat_at = {} WHERE id = {}",
            conn.now_expr(),
            conn.placeholder(1)
        ),
        &[DbValue::Text(id.to_string())],
    )
    .context("Failed to update heartbeat")?;
    Ok(())
}

/// Find jobs that are marked as running but have a stale heartbeat.
pub fn find_stale_jobs(conn: &dyn DbConnection, stale_threshold_secs: u64) -> Result<Vec<JobRun>> {
    let (offset_sql, offset_param) = conn.date_offset_expr(stale_threshold_secs as i64, 1);
    let rows = conn.query_all(
        &format!(
            "SELECT id, slug, status, queue, data, result, error, attempt, max_attempts,
                    scheduled_by, created_at, started_at, completed_at, heartbeat_at, retry_after
             FROM _crap_jobs
             WHERE status = 'running'
               AND (heartbeat_at IS NULL OR heartbeat_at < {})",
            offset_sql
        ),
        &[offset_param],
    )?;

    rows.iter().map(row_to_job_run).collect()
}

/// Count running jobs, optionally filtered by slug.
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
    match row.as_ref().and_then(|r| r.get_value(0)) {
        Some(DbValue::Integer(n)) => Ok(*n),
        _ => Ok(0),
    }
}

/// Count running jobs per slug, returned as a HashMap.
pub fn count_running_per_slug(
    conn: &dyn DbConnection,
) -> Result<std::collections::HashMap<String, i64>> {
    let rows = conn.query_all(
        "SELECT slug, COUNT(*) FROM _crap_jobs WHERE status = 'running' GROUP BY slug",
        &[],
    )?;
    let mut map = std::collections::HashMap::new();
    for row in rows {
        let slug = match row.get_value(0) {
            Some(DbValue::Text(s)) => s.clone(),
            _ => continue,
        };
        let count = match row.get_value(1) {
            Some(DbValue::Integer(n)) => *n,
            _ => 0,
        };
        map.insert(slug, count);
    }
    Ok(map)
}

/// List job runs with optional filters.
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
        sql.push_str(&format!(" AND slug = {}", conn.placeholder(params.len())));
    }
    if let Some(st) = status {
        params.push(DbValue::Text(st.to_string()));
        sql.push_str(&format!(" AND status = {}", conn.placeholder(params.len())));
    }

    params.push(DbValue::Integer(limit));
    sql.push_str(&format!(
        " ORDER BY created_at DESC LIMIT {}",
        conn.placeholder(params.len())
    ));
    params.push(DbValue::Integer(offset));
    sql.push_str(&format!(" OFFSET {}", conn.placeholder(params.len())));

    let rows = conn.query_all(&sql, &params)?;
    rows.iter().map(row_to_job_run).collect()
}

/// Get a single job run by ID.
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

    match row {
        Some(r) => Ok(Some(row_to_job_run(&r)?)),
        None => Ok(None),
    }
}

/// Delete completed/failed job runs older than the given threshold.
/// Returns the number of rows deleted.
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

pub fn purge_old_jobs(conn: &dyn DbConnection, older_than_secs: u64) -> Result<i64> {
    let (offset_sql, offset_param) = conn.date_offset_expr(older_than_secs as i64, 1);
    let deleted = conn.execute(
        &format!(
            "DELETE FROM _crap_jobs
             WHERE status IN ('completed', 'failed', 'stale')
               AND created_at < {}",
            offset_sql
        ),
        &[offset_param],
    )? as i64;

    Ok(deleted)
}

/// Count failed jobs within a recent time window (in seconds).
pub fn count_failed_since(conn: &dyn DbConnection, since_secs: u64) -> Result<i64> {
    let (offset_sql, offset_param) = conn.date_offset_expr(since_secs as i64, 1);
    let row = conn.query_one(
        &format!(
            "SELECT COUNT(*) FROM _crap_jobs
             WHERE status = 'failed'
               AND completed_at >= {}",
            offset_sql
        ),
        &[offset_param],
    )?;
    match row.as_ref().and_then(|r| r.get_value(0)) {
        Some(DbValue::Integer(n)) => Ok(*n),
        _ => Ok(0),
    }
}

/// Count pending jobs that have been waiting longer than the given threshold (in seconds).
pub fn count_pending_older_than(conn: &dyn DbConnection, older_than_secs: u64) -> Result<i64> {
    let (offset_sql, offset_param) = conn.date_offset_expr(older_than_secs as i64, 1);
    let row = conn.query_one(
        &format!(
            "SELECT COUNT(*) FROM _crap_jobs
             WHERE status = 'pending'
               AND created_at < {}",
            offset_sql
        ),
        &[offset_param],
    )?;
    match row.as_ref().and_then(|r| r.get_value(0)) {
        Some(DbValue::Integer(n)) => Ok(*n),
        _ => Ok(0),
    }
}

/// Get the most recent completed run for a given job slug.
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

    match row {
        Some(r) => Ok(Some(row_to_job_run(&r)?)),
        None => Ok(None),
    }
}

/// Mark a running job as stale.
pub fn mark_stale(conn: &dyn DbConnection, id: &str, error: &str) -> Result<()> {
    let (p1, p2) = (conn.placeholder(1), conn.placeholder(2));
    conn.execute(
        &format!(
            "UPDATE _crap_jobs SET status = 'stale', error = {p2}, completed_at = {}
         WHERE id = {p1}",
            conn.now_expr()
        ),
        &[
            DbValue::Text(id.to_string()),
            DbValue::Text(error.to_string()),
        ],
    )?;

    Ok(())
}

fn row_to_job_run(row: &DbRow) -> Result<JobRun> {
    let get_text = |idx: usize, default: &str| -> String {
        match row.get_value(idx) {
            Some(DbValue::Text(s)) => s.clone(),
            _ => default.to_string(),
        }
    };
    let get_opt_text = |idx: usize| -> Option<String> {
        match row.get_value(idx) {
            Some(DbValue::Text(s)) => Some(s.clone()),
            _ => None,
        }
    };
    let get_i64 = |idx: usize| -> i64 {
        match row.get_value(idx) {
            Some(DbValue::Integer(n)) => *n,
            _ => 0,
        }
    };

    let id = get_text(0, "");
    let slug = get_text(1, "");
    let status_str = get_text(2, "pending");
    let status = JobStatus::from_name(&status_str).unwrap_or(JobStatus::Pending);

    let mut b = JobRun::builder(id, slug)
        .status(status)
        .queue(get_text(3, "default"))
        .data(get_text(4, "{}"))
        .attempt(get_i64(7) as u32)
        .max_attempts(get_i64(8) as u32);

    if let Some(r) = get_opt_text(5) {
        b = b.result(r);
    }
    if let Some(e) = get_opt_text(6) {
        b = b.error(e);
    }
    if let Some(sb) = get_opt_text(9) {
        b = b.scheduled_by(sb);
    }
    if let Some(ca) = get_opt_text(10) {
        b = b.created_at(ca);
    }
    if let Some(sa) = get_opt_text(11) {
        b = b.started_at(sa);
    }
    if let Some(ca) = get_opt_text(12) {
        b = b.completed_at(ca);
    }
    if let Some(ha) = get_opt_text(13) {
        b = b.heartbeat_at(ha);
    }
    if let Some(ra) = get_opt_text(14) {
        b = b.retry_after(ra);
    }

    Ok(b.build())
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
            "CREATE TABLE _crap_jobs (
                id TEXT PRIMARY KEY,
                slug TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'pending',
                queue TEXT NOT NULL DEFAULT 'default',
                data TEXT DEFAULT '{}',
                result TEXT,
                error TEXT,
                attempt INTEGER NOT NULL DEFAULT 0,
                max_attempts INTEGER NOT NULL DEFAULT 1,
                scheduled_by TEXT,
                created_at TEXT DEFAULT (datetime('now')),
                started_at TEXT,
                completed_at TEXT,
                heartbeat_at TEXT,
                retry_after TEXT
            );
            CREATE INDEX idx_crap_jobs_status ON _crap_jobs(status);
            CREATE INDEX idx_crap_jobs_queue ON _crap_jobs(queue, status);
            CREATE INDEX idx_crap_jobs_slug ON _crap_jobs(slug, status);",
        )
        .unwrap();
        (dir, conn)
    }

    #[test]
    fn test_insert_and_get_job() {
        let (_dir, conn) = setup_db();
        let job = insert_job(&conn, "test_job", "{}", "manual", 1, "default").unwrap();
        assert_eq!(job.slug, "test_job");
        assert_eq!(job.status, JobStatus::Pending);

        let fetched = get_job_run(&conn, &job.id).unwrap().unwrap();
        assert_eq!(fetched.slug, "test_job");
        assert_eq!(fetched.status, JobStatus::Pending);
    }

    #[test]
    fn test_claim_pending_jobs() {
        let (_dir, conn) = setup_db();
        insert_job(&conn, "job_a", "{}", "cron", 1, "default").unwrap();
        insert_job(&conn, "job_b", "{}", "cron", 1, "default").unwrap();

        let running = std::collections::HashMap::new();
        let conc = std::collections::HashMap::new();
        let claimed = claim_pending_jobs(&conn, 10, &running, &conc).unwrap();
        assert_eq!(claimed.len(), 2);
        assert_eq!(claimed[0].status, JobStatus::Running);

        // No more pending
        let claimed2 = claim_pending_jobs(&conn, 10, &running, &conc).unwrap();
        assert_eq!(claimed2.len(), 0);
    }

    #[test]
    fn test_claim_respects_concurrency() {
        let (_dir, conn) = setup_db();
        insert_job(&conn, "limited", "{}", "cron", 1, "default").unwrap();
        insert_job(&conn, "limited", "{}", "cron", 1, "default").unwrap();

        let running = std::collections::HashMap::new();
        let mut conc = std::collections::HashMap::new();
        conc.insert("limited".to_string(), 1u32);

        let claimed = claim_pending_jobs(&conn, 10, &running, &conc).unwrap();
        assert_eq!(claimed.len(), 1);
    }

    #[test]
    fn test_complete_job() {
        let (_dir, conn) = setup_db();
        let job = insert_job(&conn, "test", "{}", "manual", 1, "default").unwrap();
        // Claim it first
        conn.execute(
            "UPDATE _crap_jobs SET status = 'running' WHERE id = ?1",
            &[DbValue::Text(job.id.clone())],
        )
        .unwrap();

        complete_job(&conn, &job.id, Some("{\"ok\":true}")).unwrap();
        let fetched = get_job_run(&conn, &job.id).unwrap().unwrap();
        assert_eq!(fetched.status, JobStatus::Completed);
        assert_eq!(fetched.result.as_deref(), Some("{\"ok\":true}"));
    }

    #[test]
    fn test_fail_job_no_retry() {
        let (_dir, conn) = setup_db();
        let job = insert_job(&conn, "test", "{}", "manual", 1, "default").unwrap();
        conn.execute(
            "UPDATE _crap_jobs SET status = 'running' WHERE id = ?1",
            &[DbValue::Text(job.id.clone())],
        )
        .unwrap();

        fail_job(&conn, &job.id, "something broke", false, 1).unwrap();
        let fetched = get_job_run(&conn, &job.id).unwrap().unwrap();
        assert_eq!(fetched.status, JobStatus::Failed);
        assert_eq!(fetched.error.as_deref(), Some("something broke"));
    }

    #[test]
    fn test_fail_job_with_retry() {
        let (_dir, conn) = setup_db();
        let job = insert_job(&conn, "test", "{}", "manual", 3, "default").unwrap();
        conn.execute(
            "UPDATE _crap_jobs SET status = 'running', attempt = 1 WHERE id = ?1",
            &[DbValue::Text(job.id.clone())],
        )
        .unwrap();

        fail_job(&conn, &job.id, "transient error", true, 1).unwrap();
        let fetched = get_job_run(&conn, &job.id).unwrap().unwrap();
        assert_eq!(fetched.status, JobStatus::Pending);
    }

    /// Regression: fail_job with retry did not clear heartbeat_at, causing the
    /// re-queued job to be immediately detected as stale by find_stale_jobs.
    #[test]
    fn test_fail_job_retry_clears_heartbeat() {
        let (_dir, conn) = setup_db();
        let job = insert_job(&conn, "test", "{}", "manual", 3, "default").unwrap();
        conn.execute(
            "UPDATE _crap_jobs SET status = 'running', attempt = 1, heartbeat_at = datetime('now') WHERE id = ?1",
            &[DbValue::Text(job.id.clone())],
        )
        .unwrap();

        fail_job(&conn, &job.id, "transient error", true, 1).unwrap();
        let fetched = get_job_run(&conn, &job.id).unwrap().unwrap();
        assert_eq!(fetched.status, JobStatus::Pending);
        assert!(
            fetched.heartbeat_at.is_none(),
            "heartbeat_at should be cleared on retry so the job is not detected as stale"
        );
    }

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

    #[test]
    fn test_mark_stale() {
        let (_dir, conn) = setup_db();
        let job = insert_job(&conn, "test", "{}", "manual", 1, "default").unwrap();
        conn.execute(
            "UPDATE _crap_jobs SET status = 'running' WHERE id = ?1",
            &[DbValue::Text(job.id.clone())],
        )
        .unwrap();

        mark_stale(&conn, &job.id, "heartbeat timeout").unwrap();
        let fetched = get_job_run(&conn, &job.id).unwrap().unwrap();
        assert_eq!(fetched.status, JobStatus::Stale);
        assert_eq!(fetched.error.as_deref(), Some("heartbeat timeout"));
    }

    #[test]
    fn test_update_heartbeat() {
        let (_dir, conn) = setup_db();
        let job = insert_job(&conn, "test", "{}", "manual", 1, "default").unwrap();
        conn.execute(
            "UPDATE _crap_jobs SET status = 'running' WHERE id = ?1",
            &[DbValue::Text(job.id.clone())],
        )
        .unwrap();

        // Update heartbeat should succeed
        update_heartbeat(&conn, &job.id).unwrap();

        let fetched = get_job_run(&conn, &job.id).unwrap().unwrap();
        assert!(
            fetched.heartbeat_at.is_some(),
            "heartbeat should be set after update"
        );
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

    #[test]
    fn test_backoff_seconds() {
        assert_eq!(backoff_seconds(0), 5);
        assert_eq!(backoff_seconds(1), 10);
        assert_eq!(backoff_seconds(2), 20);
        assert_eq!(backoff_seconds(3), 40);
        assert_eq!(backoff_seconds(4), 80);
        assert_eq!(backoff_seconds(5), 160);
        assert_eq!(backoff_seconds(6), 300);
        // Capped at 300
        assert_eq!(backoff_seconds(7), 300);
        assert_eq!(backoff_seconds(100), 300);
    }

    /// Regression: fail_job with retry did not set retry_after, causing immediate re-execution.
    #[test]
    fn test_fail_job_retry_sets_retry_after() {
        let (_dir, conn) = setup_db();
        let job = insert_job(&conn, "test", "{}", "manual", 3, "default").unwrap();
        conn.execute(
            "UPDATE _crap_jobs SET status = 'running', attempt = 1 WHERE id = ?1",
            &[DbValue::Text(job.id.clone())],
        )
        .unwrap();

        fail_job(&conn, &job.id, "transient error", true, 1).unwrap();
        let fetched = get_job_run(&conn, &job.id).unwrap().unwrap();
        assert_eq!(fetched.status, JobStatus::Pending);
        assert!(
            fetched.retry_after.is_some(),
            "retry_after should be set for backoff"
        );
    }

    /// Regression: claim_pending_jobs should skip jobs whose retry_after is in the future.
    #[test]
    fn test_claim_skips_jobs_with_future_retry_after() {
        let (_dir, conn) = setup_db();
        insert_job(&conn, "test", "{}", "manual", 3, "default").unwrap();

        // Set retry_after far in the future
        conn.execute(
            "UPDATE _crap_jobs SET retry_after = datetime('now', '+3600 seconds')",
            &[],
        )
        .unwrap();

        let running = std::collections::HashMap::new();
        let conc = std::collections::HashMap::new();
        let claimed = claim_pending_jobs(&conn, 10, &running, &conc).unwrap();
        assert_eq!(
            claimed.len(),
            0,
            "should not claim job with future retry_after"
        );
    }

    /// Jobs with retry_after in the past should be claimable.
    #[test]
    fn test_claim_picks_up_jobs_with_past_retry_after() {
        let (_dir, conn) = setup_db();
        insert_job(&conn, "test", "{}", "manual", 3, "default").unwrap();

        // Set retry_after in the past
        conn.execute(
            "UPDATE _crap_jobs SET retry_after = datetime('now', '-10 seconds')",
            &[],
        )
        .unwrap();

        let running = std::collections::HashMap::new();
        let conc = std::collections::HashMap::new();
        let claimed = claim_pending_jobs(&conn, 10, &running, &conc).unwrap();
        assert_eq!(claimed.len(), 1, "should claim job with past retry_after");
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
