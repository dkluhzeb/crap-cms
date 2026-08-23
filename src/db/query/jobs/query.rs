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

/// Count currently-running jobs grouped by queue name. Used by the
/// scheduler to enforce per-queue concurrency caps
/// (`[jobs.queues.<name>] concurrency = N`).
///
/// The count is computed across the whole DB, so per-queue caps are
/// cluster-wide in a multi-server deployment.
///
/// # Errors
///
/// Returns a backend error if the COUNT query fails.
pub fn count_running_per_queue(conn: &dyn DbConnection) -> Result<HashMap<String, i64>> {
    let rows = conn.query_all(
        "SELECT queue, COUNT(*) FROM _crap_jobs WHERE status = 'running' GROUP BY queue",
        &[],
    )?;
    let mut map = HashMap::new();

    for row in rows {
        let Some(queue) = row.opt_text_at(0) else {
            continue;
        };
        let count = row.i64_at(1).unwrap_or(0);

        map.insert(queue, count);
    }

    Ok(map)
}

/// Full job-run column list, shared by the list/count query builders.
const RUN_COLUMNS: &str = "id, slug, status, queue, data, result, error, attempt, max_attempts, \
     scheduled_by, created_at, started_at, completed_at, heartbeat_at, retry_after, \
     priority, unique_key";

/// Slug predicate for job-run reads. `In` carries an access allowlist —
/// the set of slugs the caller is permitted to read.
#[derive(Clone, Copy)]
enum SlugPred<'a> {
    Any,
    One(&'a str),
    In(&'a [String]),
}

impl SlugPred<'_> {
    /// Append the slug `WHERE` fragment to `sql`/`params`. Returns `false`
    /// when the predicate matches nothing (an empty `In` allowlist), so the
    /// caller can short-circuit instead of emitting invalid `IN ()` SQL.
    fn append(&self, sql: &mut String, params: &mut Vec<DbValue>, conn: &dyn DbConnection) -> bool {
        match self {
            SlugPred::Any => true,
            SlugPred::One(s) => {
                params.push(DbValue::Text((*s).to_string()));
                let _ = write!(sql, " AND slug = {}", conn.placeholder(params.len()));
                true
            }
            SlugPred::In(slugs) => {
                if slugs.is_empty() {
                    return false;
                }

                sql.push_str(" AND slug IN (");
                for (i, s) in slugs.iter().enumerate() {
                    params.push(DbValue::Text(s.clone()));
                    if i > 0 {
                        sql.push_str(", ");
                    }
                    let _ = write!(sql, "{}", conn.placeholder(params.len()));
                }
                sql.push(')');
                true
            }
        }
    }
}

/// Count job runs under the given slug predicate + optional status.
fn count_runs(conn: &dyn DbConnection, slug: SlugPred, status: Option<JobStatus>) -> Result<i64> {
    let mut sql = String::from("SELECT COUNT(*) FROM _crap_jobs WHERE 1=1");
    let mut params: Vec<DbValue> = Vec::new();

    if !slug.append(&mut sql, &mut params, conn) {
        return Ok(0);
    }

    if let Some(st) = status {
        params.push(DbValue::Text(st.as_str().to_string()));
        let _ = write!(sql, " AND status = {}", conn.placeholder(params.len()));
    }

    let row = conn.query_one(&sql, &params)?;

    Ok(row.as_ref().and_then(|r| r.i64_at(0)).unwrap_or(0))
}

/// List job runs under the given slug predicate + optional status.
fn query_runs(
    conn: &dyn DbConnection,
    slug: SlugPred,
    status: Option<JobStatus>,
    limit: i64,
    offset: i64,
) -> Result<Vec<JobRun>> {
    let mut sql = format!("SELECT {RUN_COLUMNS} FROM _crap_jobs WHERE 1=1");
    let mut params: Vec<DbValue> = Vec::new();

    if !slug.append(&mut sql, &mut params, conn) {
        return Ok(Vec::new());
    }

    if let Some(st) = status {
        params.push(DbValue::Text(st.as_str().to_string()));
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

/// Count job runs with optional filters (same WHERE clause as [`list_job_runs`]).
///
/// # Errors
///
/// Returns a backend error if the COUNT query fails.
pub fn count_job_runs(
    conn: &dyn DbConnection,
    slug: Option<&str>,
    status: Option<JobStatus>,
) -> Result<i64> {
    count_runs(conn, slug.map_or(SlugPred::Any, SlugPred::One), status)
}

/// Count job runs restricted to an access allowlist of slugs. An empty
/// allowlist yields `0`.
///
/// # Errors
///
/// Returns a backend error if the COUNT query fails.
pub fn count_job_runs_in(
    conn: &dyn DbConnection,
    slugs: &[String],
    status: Option<JobStatus>,
) -> Result<i64> {
    count_runs(conn, SlugPred::In(slugs), status)
}

/// List job runs with optional filters.
///
/// # Errors
///
/// Returns a backend error if the SELECT query fails or any row fails to parse.
pub fn list_job_runs(
    conn: &dyn DbConnection,
    slug: Option<&str>,
    status: Option<JobStatus>,
    limit: i64,
    offset: i64,
) -> Result<Vec<JobRun>> {
    query_runs(
        conn,
        slug.map_or(SlugPred::Any, SlugPred::One),
        status,
        limit,
        offset,
    )
}

/// List job runs restricted to an access allowlist of slugs. An empty
/// allowlist yields no rows.
///
/// # Errors
///
/// Returns a backend error if the SELECT query fails or any row fails to parse.
pub fn list_job_runs_in(
    conn: &dyn DbConnection,
    slugs: &[String],
    status: Option<JobStatus>,
    limit: i64,
    offset: i64,
) -> Result<Vec<JobRun>> {
    query_runs(conn, SlugPred::In(slugs), status, limit, offset)
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
                scheduled_by, created_at, started_at, completed_at, heartbeat_at, retry_after,
                priority, unique_key
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
            "SELECT {RUN_COLUMNS}
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
                scheduled_by, created_at, started_at, completed_at, heartbeat_at, retry_after,
                priority, unique_key
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

/// Build a `JobRun` from a wide DB row of the full [`RUN_COLUMNS`] set
/// (17 columns; reads `priority` at index 15 and `unique_key` at 16).
///
/// All column accessors fall back to sensible defaults (empty string,
/// `JobStatus::Pending`, `0` for counters), so this never fails — a
/// malformed row produces a logically-empty `JobRun` rather than an error.
/// Callers must therefore `SELECT` the columns in [`RUN_COLUMNS`] order.
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
        )
        .priority(
            row.i64_at(15)
                .and_then(|v| i32::try_from(v).ok())
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

    if let Some(uk) = row.opt_text_at(16) {
        b = b.unique_key(uk);
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
        insert_job(&conn, "job_a", "{}", "cron", 1, "default", 0).unwrap();
        insert_job(&conn, "job_b", "{}", "cron", 1, "default", 0).unwrap();
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
        insert_job(&conn, "job_a", "{}", "cron", 1, "default", 0).unwrap();
        insert_job(&conn, "job_b", "{}", "manual", 1, "default", 0).unwrap();

        let all = list_job_runs(&conn, None, None, 100, 0).unwrap();
        assert_eq!(all.len(), 2);

        let filtered = list_job_runs(&conn, Some("job_a"), None, 100, 0).unwrap();
        assert_eq!(filtered.len(), 1);
    }

    #[test]
    fn list_and_count_in_filter_to_allowlist() {
        let (_dir, conn) = setup_db();
        insert_job(&conn, "job_a", "{}", "cron", 1, "default", 0).unwrap();
        insert_job(&conn, "job_b", "{}", "manual", 1, "default", 0).unwrap();
        insert_job(&conn, "job_c", "{}", "manual", 1, "default", 0).unwrap();

        let allowed = vec!["job_a".to_string(), "job_c".to_string()];
        let runs = list_job_runs_in(&conn, &allowed, None, 100, 0).unwrap();
        let slugs: Vec<&str> = runs.iter().map(|r| r.slug.as_str()).collect();
        assert_eq!(runs.len(), 2);
        assert!(slugs.contains(&"job_a") && slugs.contains(&"job_c"));
        assert!(!slugs.contains(&"job_b"));
        assert_eq!(count_job_runs_in(&conn, &allowed, None).unwrap(), 2);
    }

    #[test]
    fn list_and_count_in_empty_allowlist_match_nothing() {
        // The empty-allowlist short-circuit must yield no rows / zero — never an
        // `IN ()` SQL error, and never (via a dropped WHERE) every row.
        let (_dir, conn) = setup_db();
        insert_job(&conn, "job_a", "{}", "cron", 1, "default", 0).unwrap();
        insert_job(&conn, "job_b", "{}", "manual", 1, "default", 0).unwrap();

        assert!(
            list_job_runs_in(&conn, &[], None, 100, 0)
                .unwrap()
                .is_empty()
        );
        assert_eq!(count_job_runs_in(&conn, &[], None).unwrap(), 0);
    }

    #[test]
    fn test_count_running_per_slug() {
        let (_dir, conn) = setup_db();
        insert_job(&conn, "job_a", "{}", "cron", 1, "default", 0).unwrap();
        insert_job(&conn, "job_a", "{}", "cron", 1, "default", 0).unwrap();
        insert_job(&conn, "job_b", "{}", "cron", 1, "default", 0).unwrap();

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
        insert_job(&conn, "job_a", "{}", "cron", 1, "default", 0).unwrap();
        insert_job(&conn, "job_b", "{}", "cron", 1, "default", 0).unwrap();
        conn.execute(
            "UPDATE _crap_jobs SET status = 'running' WHERE slug = 'job_a'",
            &[],
        )
        .unwrap();

        let running = list_job_runs(&conn, None, Some(JobStatus::Running), 100, 0).unwrap();
        assert_eq!(running.len(), 1);
        assert_eq!(running[0].slug, "job_a");

        let pending = list_job_runs(&conn, None, Some(JobStatus::Pending), 100, 0).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].slug, "job_b");

        // A valid status with no rows returns empty — the mechanism the service
        // layer relies on to map an unknown status string to an empty page.
        let failed = list_job_runs(&conn, None, Some(JobStatus::Failed), 100, 0).unwrap();
        assert!(failed.is_empty());
    }

    #[test]
    fn test_find_stale_jobs() {
        let (_dir, conn) = setup_db();
        let job = insert_job(&conn, "test", "{}", "manual", 1, "default", 0).unwrap();
        // Set job as running with a stale heartbeat
        conn.execute(
            "UPDATE _crap_jobs SET status = 'running', heartbeat_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-3600 seconds') WHERE id = ?1",
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
            "INSERT INTO _crap_jobs (id, slug, status, completed_at) VALUES ('f1', 'test', 'failed', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
            &[],
        ).unwrap();
        // Insert an old failed job
        conn.execute(
            "INSERT INTO _crap_jobs (id, slug, status, completed_at) VALUES ('f2', 'test', 'failed', strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-48 hours'))",
            &[],
        ).unwrap();
        // Insert a completed job (should not count)
        conn.execute(
            "INSERT INTO _crap_jobs (id, slug, status, completed_at) VALUES ('c1', 'test', 'completed', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
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
            "INSERT INTO _crap_jobs (id, slug, status, created_at) VALUES ('p1', 'test', 'pending', strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-600 seconds'))",
            &[],
        ).unwrap();
        // Insert a recent pending job
        conn.execute(
            "INSERT INTO _crap_jobs (id, slug, status, created_at) VALUES ('p2', 'test', 'pending', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
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
            "INSERT INTO _crap_jobs (id, slug, status, completed_at) VALUES ('c1', 'test', 'completed', strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-1 hour'))",
            &[],
        ).unwrap();
        conn.execute(
            "INSERT INTO _crap_jobs (id, slug, status, completed_at) VALUES ('c2', 'test', 'completed', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
            &[],
        ).unwrap();

        let last = last_completed_run(&conn, "test").unwrap().unwrap();
        assert_eq!(last.id, "c2", "should return the most recent completed run");

        // Different slug should return None
        let other = last_completed_run(&conn, "other").unwrap();
        assert!(other.is_none());
    }
}
