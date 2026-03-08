//! CRUD query functions for the `_crap_jobs` table.

use anyhow::{Context as _, Result};
use crate::core::job::{JobRun, JobStatus};

/// Insert a new pending job run.
pub fn insert_job(
    conn: &rusqlite::Connection,
    slug: &str,
    data: &str,
    scheduled_by: &str,
    max_attempts: u32,
    queue: &str,
) -> Result<JobRun> {
    let id = nanoid::nanoid!();
    conn.execute(
        "INSERT INTO _crap_jobs (id, slug, status, queue, data, max_attempts, scheduled_by)
         VALUES (?1, ?2, 'pending', ?3, ?4, ?5, ?6)",
        rusqlite::params![id, slug, queue, data, max_attempts, scheduled_by],
    ).context("Failed to insert job run")?;

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
    conn: &rusqlite::Connection,
    limit: usize,
    running_counts: &std::collections::HashMap<String, i64>,
    job_concurrency: &std::collections::HashMap<String, u32>,
) -> Result<Vec<JobRun>> {
    let mut stmt = conn.prepare(
        "SELECT id, slug, queue, data, attempt, max_attempts, scheduled_by, created_at
         FROM _crap_jobs
         WHERE status = 'pending'
         ORDER BY created_at ASC
         LIMIT ?1"
    )?;

    let rows: Vec<(String, String, String, String, u32, u32, Option<String>, Option<String>)> = stmt
        .query_map([limit as i64 * 2], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get::<_, i64>(4)? as u32,
                row.get::<_, i64>(5)? as u32,
                row.get(6)?,
                row.get(7)?,
            ))
        })?
        .filter_map(|r| r.ok())
        .collect();

    let mut claimed = Vec::new();
    let mut extra_running: std::collections::HashMap<String, i64> = std::collections::HashMap::new();

    for (id, slug, queue, data, attempt, max_attempts, scheduled_by, created_at) in rows {
        if claimed.len() >= limit {
            break;
        }

        // Check per-job concurrency
        let max_conc = job_concurrency.get(&slug).copied().unwrap_or(1) as i64;
        let current = running_counts.get(&slug).copied().unwrap_or(0)
            + extra_running.get(&slug).copied().unwrap_or(0);
        if current >= max_conc {
            continue;
        }

        // Claim the job
        let affected = conn.execute(
            "UPDATE _crap_jobs SET status = 'running', started_at = datetime('now'), heartbeat_at = datetime('now'), attempt = attempt + 1
             WHERE id = ?1 AND status = 'pending'",
            [&id],
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
pub fn complete_job(conn: &rusqlite::Connection, id: &str, result_json: Option<&str>) -> Result<()> {
    conn.execute(
        "UPDATE _crap_jobs SET status = 'completed', result = ?2, completed_at = datetime('now')
         WHERE id = ?1",
        rusqlite::params![id, result_json],
    ).context("Failed to complete job")?;
    Ok(())
}

/// Mark a job as failed. If should_retry is true and attempt < max_attempts, resets to pending.
pub fn fail_job(conn: &rusqlite::Connection, id: &str, error: &str, should_retry: bool) -> Result<()> {
    if should_retry {
        conn.execute(
            "UPDATE _crap_jobs SET status = 'pending', error = ?2, started_at = NULL, completed_at = NULL
             WHERE id = ?1",
            rusqlite::params![id, error],
        ).context("Failed to retry job")?;
    } else {
        conn.execute(
            "UPDATE _crap_jobs SET status = 'failed', error = ?2, completed_at = datetime('now')
             WHERE id = ?1",
            rusqlite::params![id, error],
        ).context("Failed to fail job")?;
    }
    Ok(())
}

/// Update the heartbeat timestamp for a running job.
pub fn update_heartbeat(conn: &rusqlite::Connection, id: &str) -> Result<()> {
    conn.execute(
        "UPDATE _crap_jobs SET heartbeat_at = datetime('now') WHERE id = ?1",
        [id],
    ).context("Failed to update heartbeat")?;
    Ok(())
}

/// Find jobs that are marked as running but have a stale heartbeat.
pub fn find_stale_jobs(conn: &rusqlite::Connection, stale_threshold_secs: u64) -> Result<Vec<JobRun>> {
    let mut stmt = conn.prepare(
        "SELECT id, slug, status, queue, data, result, error, attempt, max_attempts,
                scheduled_by, created_at, started_at, completed_at, heartbeat_at
         FROM _crap_jobs
         WHERE status = 'running'
           AND (heartbeat_at IS NULL OR heartbeat_at < datetime('now', ?1))"
    )?;

    let threshold = format!("-{} seconds", stale_threshold_secs);
    let jobs = stmt.query_map([&threshold], row_to_job_run)?
        .filter_map(|r| r.ok())
        .collect();

    Ok(jobs)
}

/// Count running jobs, optionally filtered by slug.
pub fn count_running(conn: &rusqlite::Connection, slug: Option<&str>) -> Result<i64> {
    match slug {
        Some(s) => {
            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM _crap_jobs WHERE status = 'running' AND slug = ?1",
                [s],
                |row| row.get(0),
            )?;
            Ok(count)
        }
        None => {
            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM _crap_jobs WHERE status = 'running'",
                [],
                |row| row.get(0),
            )?;
            Ok(count)
        }
    }
}

/// Count running jobs per slug, returned as a HashMap.
pub fn count_running_per_slug(conn: &rusqlite::Connection) -> Result<std::collections::HashMap<String, i64>> {
    let mut stmt = conn.prepare(
        "SELECT slug, COUNT(*) FROM _crap_jobs WHERE status = 'running' GROUP BY slug"
    )?;
    let mut map = std::collections::HashMap::new();
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    for row in rows {
        let (slug, count) = row?;
        map.insert(slug, count);
    }
    Ok(map)
}

/// List job runs with optional filters.
pub fn list_job_runs(
    conn: &rusqlite::Connection,
    slug: Option<&str>,
    status: Option<&str>,
    limit: i64,
    offset: i64,
) -> Result<Vec<JobRun>> {
    let mut sql = String::from(
        "SELECT id, slug, status, queue, data, result, error, attempt, max_attempts,
                scheduled_by, created_at, started_at, completed_at, heartbeat_at
         FROM _crap_jobs WHERE 1=1"
    );
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

    if let Some(s) = slug {
        params.push(Box::new(s.to_string()));
        sql.push_str(&format!(" AND slug = ?{}", params.len()));
    }
    if let Some(st) = status {
        params.push(Box::new(st.to_string()));
        sql.push_str(&format!(" AND status = ?{}", params.len()));
    }

    params.push(Box::new(limit));
    sql.push_str(&format!(" ORDER BY created_at DESC LIMIT ?{}", params.len()));
    params.push(Box::new(offset));
    sql.push_str(&format!(" OFFSET ?{}", params.len()));

    let mut stmt = conn.prepare(&sql)?;
    let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let jobs = stmt.query_map(param_refs.as_slice(), row_to_job_run)?
        .filter_map(|r| r.ok())
        .collect();

    Ok(jobs)
}

/// Get a single job run by ID.
pub fn get_job_run(conn: &rusqlite::Connection, id: &str) -> Result<Option<JobRun>> {
    let mut stmt = conn.prepare(
        "SELECT id, slug, status, queue, data, result, error, attempt, max_attempts,
                scheduled_by, created_at, started_at, completed_at, heartbeat_at
         FROM _crap_jobs WHERE id = ?1"
    )?;

    let mut rows = stmt.query_map([id], row_to_job_run)?;
    match rows.next() {
        Some(Ok(job)) => Ok(Some(job)),
        Some(Err(e)) => Err(e.into()),
        None => Ok(None),
    }
}

/// Delete completed/failed job runs older than the given threshold.
/// Returns the number of rows deleted.
pub fn purge_old_jobs(conn: &rusqlite::Connection, older_than_secs: u64) -> Result<i64> {
    let threshold = format!("-{} seconds", older_than_secs);
    let deleted = conn.execute(
        "DELETE FROM _crap_jobs
         WHERE status IN ('completed', 'failed', 'stale')
           AND created_at < datetime('now', ?1)",
        [&threshold],
    )? as i64;
    Ok(deleted)
}

/// Count failed jobs within a recent time window (in seconds).
pub fn count_failed_since(conn: &rusqlite::Connection, since_secs: u64) -> Result<i64> {
    let threshold = format!("-{} seconds", since_secs);
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM _crap_jobs
         WHERE status = 'failed'
           AND completed_at >= datetime('now', ?1)",
        [&threshold],
        |row| row.get(0),
    )?;
    Ok(count)
}

/// Count pending jobs that have been waiting longer than the given threshold (in seconds).
pub fn count_pending_older_than(conn: &rusqlite::Connection, older_than_secs: u64) -> Result<i64> {
    let threshold = format!("-{} seconds", older_than_secs);
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM _crap_jobs
         WHERE status = 'pending'
           AND created_at < datetime('now', ?1)",
        [&threshold],
        |row| row.get(0),
    )?;
    Ok(count)
}

/// Get the most recent completed run for a given job slug.
pub fn last_completed_run(conn: &rusqlite::Connection, slug: &str) -> Result<Option<JobRun>> {
    let mut stmt = conn.prepare(
        "SELECT id, slug, status, queue, data, result, error, attempt, max_attempts,
                scheduled_by, created_at, started_at, completed_at, heartbeat_at
         FROM _crap_jobs
         WHERE slug = ?1 AND status = 'completed'
         ORDER BY completed_at DESC
         LIMIT 1"
    )?;
    let mut rows = stmt.query_map([slug], row_to_job_run)?;
    match rows.next() {
        Some(Ok(job)) => Ok(Some(job)),
        Some(Err(e)) => Err(e.into()),
        None => Ok(None),
    }
}

/// Mark a running job as stale.
pub fn mark_stale(conn: &rusqlite::Connection, id: &str, error: &str) -> Result<()> {
    conn.execute(
        "UPDATE _crap_jobs SET status = 'stale', error = ?2, completed_at = datetime('now')
         WHERE id = ?1",
        rusqlite::params![id, error],
    )?;
    Ok(())
}

fn row_to_job_run(row: &rusqlite::Row) -> rusqlite::Result<JobRun> {
    let id: String = row.get(0)?;
    let slug: String = row.get(1)?;
    let status_str: String = row.get(2)?;
    let status = JobStatus::from_str(&status_str).unwrap_or(JobStatus::Pending);
    let mut b = JobRun::builder(id, slug)
        .status(status)
        .queue(row.get::<_, String>(3)?)
        .data(row.get::<_, String>(4).unwrap_or_else(|_| "{}".to_string()))
        .attempt(row.get::<_, i64>(7).unwrap_or(0) as u32)
        .max_attempts(row.get::<_, i64>(8).unwrap_or(1) as u32);
    if let Some(r) = row.get::<_, Option<String>>(5)? {
        b = b.result(r);
    }
    if let Some(e) = row.get::<_, Option<String>>(6)? {
        b = b.error(e);
    }
    if let Some(sb) = row.get::<_, Option<String>>(9)? {
        b = b.scheduled_by(sb);
    }
    if let Some(ca) = row.get::<_, Option<String>>(10)? {
        b = b.created_at(ca);
    }
    if let Some(sa) = row.get::<_, Option<String>>(11)? {
        b = b.started_at(sa);
    }
    if let Some(ca) = row.get::<_, Option<String>>(12)? {
        b = b.completed_at(ca);
    }
    if let Some(ha) = row.get::<_, Option<String>>(13)? {
        b = b.heartbeat_at(ha);
    }
    Ok(b.build())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_db() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
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
                heartbeat_at TEXT
            );
            CREATE INDEX idx_crap_jobs_status ON _crap_jobs(status);
            CREATE INDEX idx_crap_jobs_queue ON _crap_jobs(queue, status);
            CREATE INDEX idx_crap_jobs_slug ON _crap_jobs(slug, status);"
        ).unwrap();
        conn
    }

    #[test]
    fn test_insert_and_get_job() {
        let conn = setup_db();
        let job = insert_job(&conn, "test_job", "{}", "manual", 1, "default").unwrap();
        assert_eq!(job.slug, "test_job");
        assert_eq!(job.status, JobStatus::Pending);

        let fetched = get_job_run(&conn, &job.id).unwrap().unwrap();
        assert_eq!(fetched.slug, "test_job");
        assert_eq!(fetched.status, JobStatus::Pending);
    }

    #[test]
    fn test_claim_pending_jobs() {
        let conn = setup_db();
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
        let conn = setup_db();
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
        let conn = setup_db();
        let job = insert_job(&conn, "test", "{}", "manual", 1, "default").unwrap();
        // Claim it first
        conn.execute("UPDATE _crap_jobs SET status = 'running' WHERE id = ?1", [&job.id]).unwrap();

        complete_job(&conn, &job.id, Some("{\"ok\":true}")).unwrap();
        let fetched = get_job_run(&conn, &job.id).unwrap().unwrap();
        assert_eq!(fetched.status, JobStatus::Completed);
        assert_eq!(fetched.result.as_deref(), Some("{\"ok\":true}"));
    }

    #[test]
    fn test_fail_job_no_retry() {
        let conn = setup_db();
        let job = insert_job(&conn, "test", "{}", "manual", 1, "default").unwrap();
        conn.execute("UPDATE _crap_jobs SET status = 'running' WHERE id = ?1", [&job.id]).unwrap();

        fail_job(&conn, &job.id, "something broke", false).unwrap();
        let fetched = get_job_run(&conn, &job.id).unwrap().unwrap();
        assert_eq!(fetched.status, JobStatus::Failed);
        assert_eq!(fetched.error.as_deref(), Some("something broke"));
    }

    #[test]
    fn test_fail_job_with_retry() {
        let conn = setup_db();
        let job = insert_job(&conn, "test", "{}", "manual", 3, "default").unwrap();
        conn.execute("UPDATE _crap_jobs SET status = 'running', attempt = 1 WHERE id = ?1", [&job.id]).unwrap();

        fail_job(&conn, &job.id, "transient error", true).unwrap();
        let fetched = get_job_run(&conn, &job.id).unwrap().unwrap();
        assert_eq!(fetched.status, JobStatus::Pending);
    }

    #[test]
    fn test_count_running() {
        let conn = setup_db();
        insert_job(&conn, "job_a", "{}", "cron", 1, "default").unwrap();
        insert_job(&conn, "job_b", "{}", "cron", 1, "default").unwrap();
        conn.execute("UPDATE _crap_jobs SET status = 'running' WHERE slug = 'job_a'", []).unwrap();

        assert_eq!(count_running(&conn, None).unwrap(), 1);
        assert_eq!(count_running(&conn, Some("job_a")).unwrap(), 1);
        assert_eq!(count_running(&conn, Some("job_b")).unwrap(), 0);
    }

    #[test]
    fn test_list_job_runs() {
        let conn = setup_db();
        insert_job(&conn, "job_a", "{}", "cron", 1, "default").unwrap();
        insert_job(&conn, "job_b", "{}", "manual", 1, "default").unwrap();

        let all = list_job_runs(&conn, None, None, 100, 0).unwrap();
        assert_eq!(all.len(), 2);

        let filtered = list_job_runs(&conn, Some("job_a"), None, 100, 0).unwrap();
        assert_eq!(filtered.len(), 1);
    }

    #[test]
    fn test_purge_old_jobs() {
        let conn = setup_db();
        // Insert a completed job with old timestamp
        conn.execute(
            "INSERT INTO _crap_jobs (id, slug, status, created_at) VALUES ('old1', 'test', 'completed', datetime('now', '-30 days'))",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO _crap_jobs (id, slug, status, created_at) VALUES ('new1', 'test', 'completed', datetime('now'))",
            [],
        ).unwrap();

        let deleted = purge_old_jobs(&conn, 86400 * 7).unwrap(); // 7 days
        assert_eq!(deleted, 1);

        let remaining = list_job_runs(&conn, None, None, 100, 0).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, "new1");
    }

    #[test]
    fn test_mark_stale() {
        let conn = setup_db();
        let job = insert_job(&conn, "test", "{}", "manual", 1, "default").unwrap();
        conn.execute("UPDATE _crap_jobs SET status = 'running' WHERE id = ?1", [&job.id]).unwrap();

        mark_stale(&conn, &job.id, "heartbeat timeout").unwrap();
        let fetched = get_job_run(&conn, &job.id).unwrap().unwrap();
        assert_eq!(fetched.status, JobStatus::Stale);
        assert_eq!(fetched.error.as_deref(), Some("heartbeat timeout"));
    }

    #[test]
    fn test_update_heartbeat() {
        let conn = setup_db();
        let job = insert_job(&conn, "test", "{}", "manual", 1, "default").unwrap();
        conn.execute("UPDATE _crap_jobs SET status = 'running' WHERE id = ?1", [&job.id]).unwrap();

        // Update heartbeat should succeed
        update_heartbeat(&conn, &job.id).unwrap();

        let fetched = get_job_run(&conn, &job.id).unwrap().unwrap();
        assert!(fetched.heartbeat_at.is_some(), "heartbeat should be set after update");
    }

    #[test]
    fn test_count_running_per_slug() {
        let conn = setup_db();
        insert_job(&conn, "job_a", "{}", "cron", 1, "default").unwrap();
        insert_job(&conn, "job_a", "{}", "cron", 1, "default").unwrap();
        insert_job(&conn, "job_b", "{}", "cron", 1, "default").unwrap();

        conn.execute("UPDATE _crap_jobs SET status = 'running' WHERE slug = 'job_a'", []).unwrap();
        conn.execute("UPDATE _crap_jobs SET status = 'running' WHERE slug = 'job_b'", []).unwrap();

        let counts = count_running_per_slug(&conn).unwrap();
        assert_eq!(counts.get("job_a").copied(), Some(2));
        assert_eq!(counts.get("job_b").copied(), Some(1));
    }

    #[test]
    fn test_get_job_run_not_found() {
        let conn = setup_db();
        let result = get_job_run(&conn, "nonexistent").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_list_job_runs_with_status_filter() {
        let conn = setup_db();
        insert_job(&conn, "job_a", "{}", "cron", 1, "default").unwrap();
        insert_job(&conn, "job_b", "{}", "cron", 1, "default").unwrap();
        conn.execute("UPDATE _crap_jobs SET status = 'running' WHERE slug = 'job_a'", []).unwrap();

        let running = list_job_runs(&conn, None, Some("running"), 100, 0).unwrap();
        assert_eq!(running.len(), 1);
        assert_eq!(running[0].slug, "job_a");

        let pending = list_job_runs(&conn, None, Some("pending"), 100, 0).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].slug, "job_b");
    }

    #[test]
    fn test_find_stale_jobs() {
        let conn = setup_db();
        let job = insert_job(&conn, "test", "{}", "manual", 1, "default").unwrap();
        // Set job as running with a stale heartbeat
        conn.execute(
            "UPDATE _crap_jobs SET status = 'running', heartbeat_at = datetime('now', '-3600 seconds') WHERE id = ?1",
            [&job.id],
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
        let conn = setup_db();
        // Insert a recently failed job
        conn.execute(
            "INSERT INTO _crap_jobs (id, slug, status, completed_at) VALUES ('f1', 'test', 'failed', datetime('now'))",
            [],
        ).unwrap();
        // Insert an old failed job
        conn.execute(
            "INSERT INTO _crap_jobs (id, slug, status, completed_at) VALUES ('f2', 'test', 'failed', datetime('now', '-48 hours'))",
            [],
        ).unwrap();
        // Insert a completed job (should not count)
        conn.execute(
            "INSERT INTO _crap_jobs (id, slug, status, completed_at) VALUES ('c1', 'test', 'completed', datetime('now'))",
            [],
        ).unwrap();

        let count = count_failed_since(&conn, 86400).unwrap(); // 24h
        assert_eq!(count, 1, "only the recent failure should count");

        let count_all = count_failed_since(&conn, 86400 * 3).unwrap(); // 3 days
        assert_eq!(count_all, 2, "both failures within 3 days");
    }

    #[test]
    fn test_count_pending_older_than() {
        let conn = setup_db();
        // Insert a pending job from 10 minutes ago
        conn.execute(
            "INSERT INTO _crap_jobs (id, slug, status, created_at) VALUES ('p1', 'test', 'pending', datetime('now', '-600 seconds'))",
            [],
        ).unwrap();
        // Insert a recent pending job
        conn.execute(
            "INSERT INTO _crap_jobs (id, slug, status, created_at) VALUES ('p2', 'test', 'pending', datetime('now'))",
            [],
        ).unwrap();

        let count = count_pending_older_than(&conn, 300).unwrap(); // 5 min
        assert_eq!(count, 1, "only the old pending job should count");

        let count_all = count_pending_older_than(&conn, 1).unwrap(); // 1 second
        assert_eq!(count_all, 1, "still just the old one");
    }

    #[test]
    fn test_last_completed_run() {
        let conn = setup_db();

        // No completed runs
        let last = last_completed_run(&conn, "test").unwrap();
        assert!(last.is_none());

        // Add a completed run
        conn.execute(
            "INSERT INTO _crap_jobs (id, slug, status, completed_at) VALUES ('c1', 'test', 'completed', datetime('now', '-1 hour'))",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO _crap_jobs (id, slug, status, completed_at) VALUES ('c2', 'test', 'completed', datetime('now'))",
            [],
        ).unwrap();

        let last = last_completed_run(&conn, "test").unwrap().unwrap();
        assert_eq!(last.id, "c2", "should return the most recent completed run");

        // Different slug should return None
        let other = last_completed_run(&conn, "other").unwrap();
        assert!(other.is_none());
    }
}
