//! Job lifecycle writes: insert, complete, fail (with retry backoff),
//! heartbeat, mark-stale.

use std::cmp;

use anyhow::{Context as _, Result};
use nanoid::nanoid;

use crate::core::JobRun;
use crate::db::query::jobs::get_job_run;
use crate::db::{DbConnection, DbValue};

/// Rich insertion options for `insert_job_with`. Use the simpler
/// positional [`insert_job`] when only the default delay / unique-key
/// behaviour is needed.
pub struct InsertJobOpts<'a> {
    pub slug: &'a str,
    pub data: &'a str,
    pub scheduled_by: &'a str,
    pub max_attempts: u32,
    pub queue: &'a str,
    pub priority: i32,
    /// Seconds to wait before this job becomes claimable. `0` =
    /// claimable immediately (default). Maps to `retry_after =
    /// now() + delay_secs` at insert time.
    pub delay_secs: u64,
    /// Optional dedup key. When `Some`, an `(slug, unique_key)` pair
    /// that already has a pending/running job in `_crap_jobs` causes
    /// the existing job to be returned in [`InsertedJob::Existing`]
    /// instead of inserting a duplicate.
    pub unique_key: Option<&'a str>,
}

/// Result of an [`insert_job_with`] call. Distinguishes a freshly
/// inserted job from one that matched an existing `unique_key`.
pub enum InsertedJob {
    /// A new job row was inserted.
    Created(JobRun),
    /// An existing pending / running job was found with the same
    /// `(slug, unique_key)` pair; no new row was inserted.
    Existing(JobRun),
}

impl InsertedJob {
    /// Consume self and return the [`JobRun`] — discards the
    /// Created/Existing distinction. Convenient for callers that
    /// don't need to know whether a dedup happened.
    #[must_use]
    pub fn into_inner(self) -> JobRun {
        match self {
            Self::Created(j) | Self::Existing(j) => j,
        }
    }
}

/// Insert a new pending job run (positional form).
///
/// `priority` orders job claiming — higher is sooner. Pass `0` for
/// the standard FIFO default; positive values jump the queue; negative
/// values run only when the queue is otherwise idle.
///
/// For `delay` / `unique_key` use [`insert_job_with`] with an
/// [`InsertJobOpts`].
///
/// # Errors
///
/// Returns a backend error if the INSERT fails.
pub fn insert_job(
    conn: &dyn DbConnection,
    slug: &str,
    data: &str,
    scheduled_by: &str,
    max_attempts: u32,
    queue: &str,
    priority: i32,
) -> Result<JobRun> {
    let opts = InsertJobOpts {
        slug,
        data,
        scheduled_by,
        max_attempts,
        queue,
        priority,
        delay_secs: 0,
        unique_key: None,
    };
    Ok(insert_job_with(conn, &opts)?.into_inner())
}

/// Insert a new pending job run with rich options
/// (`delay`, `unique_key`). Returns [`InsertedJob::Existing`] when a
/// matching `unique_key` is already queued/running.
///
/// **Race safety**: when `unique_key` is set, the INSERT uses
/// `ON CONFLICT DO NOTHING` against the partial unique index on
/// `(slug, unique_key) WHERE status IN ('pending', 'running')`. Two
/// concurrent workers calling `insert_job_with` with the same key
/// produce exactly one row and both observe the same final id —
/// the late writer sees `RETURNING` come back empty and falls back
/// to `find_active_by_unique_key`.
///
/// # Errors
///
/// Returns a backend error if the INSERT fails or the unique-key
/// existing-row lookup fails.
pub fn insert_job_with(conn: &dyn DbConnection, opts: &InsertJobOpts) -> Result<InsertedJob> {
    let id = nanoid!();
    let want_delay = opts.delay_secs > 0;
    let want_unique = opts.unique_key.is_some();

    // SQL fragments built up by which optional features are active.
    let (p1, p2, p3, p4, p5, p6, p7, p8) = (
        conn.placeholder(1),
        conn.placeholder(2),
        conn.placeholder(3),
        conn.placeholder(4),
        conn.placeholder(5),
        conn.placeholder(6),
        conn.placeholder(7),
        conn.placeholder(8),
    );

    // `retry_after` only set when delay > 0. `date_offset_expr` with a
    // negative input produces a `+N seconds` param (future timestamp);
    // see backend impls for the sign convention.
    let delay_i64 = i64::try_from(opts.delay_secs).unwrap_or(i64::MAX);
    let (retry_after_col, retry_after_value, retry_after_param) = if want_delay {
        let (offset_sql, offset_param) = conn.date_offset_expr(-delay_i64, 9);
        (
            ", retry_after",
            format!(", {offset_sql}"),
            Some(offset_param),
        )
    } else {
        ("", String::new(), None)
    };

    let columns = format!(
        "id, slug, status, queue, data, max_attempts, scheduled_by, priority, unique_key{retry_after_col}"
    );
    let values =
        format!("{p1}, {p2}, 'pending', {p3}, {p4}, {p5}, {p6}, {p7}, {p8}{retry_after_value}");

    // ON CONFLICT clause matches the partial unique index
    // (`idx_crap_jobs_unique_active`). Without `unique_key`, no
    // conflict is possible (NULL keys are excluded by the partial
    // index's WHERE), so the clause is harmless but we skip it for
    // SQL hygiene.
    let on_conflict = if want_unique {
        " ON CONFLICT (slug, unique_key) \
          WHERE unique_key IS NOT NULL AND status IN ('pending', 'running') \
          DO NOTHING"
    } else {
        ""
    };

    let mut params = vec![
        DbValue::Text(id.clone()),
        DbValue::Text(opts.slug.to_string()),
        DbValue::Text(opts.queue.to_string()),
        DbValue::Text(opts.data.to_string()),
        DbValue::Integer(i64::from(opts.max_attempts)),
        DbValue::Text(opts.scheduled_by.to_string()),
        DbValue::Integer(i64::from(opts.priority)),
        opts.unique_key
            .map_or(DbValue::Null, |k| DbValue::Text(k.to_string())),
    ];
    if let Some(param) = retry_after_param {
        params.push(param);
    }

    let sql =
        format!("INSERT INTO _crap_jobs ({columns}) VALUES ({values}){on_conflict} RETURNING id");
    let inserted_row = conn
        .query_one(&sql, &params)
        .context("Failed to insert job run")?;

    if inserted_row.is_some() {
        // RETURNING produced a row → INSERT succeeded.
        let mut run_builder = JobRun::builder(id, opts.slug)
            .queue(opts.queue)
            .data(opts.data)
            .max_attempts(opts.max_attempts)
            .scheduled_by(opts.scheduled_by)
            .priority(opts.priority);
        if let Some(key) = opts.unique_key {
            run_builder = run_builder.unique_key(key);
        }
        return Ok(InsertedJob::Created(run_builder.build()));
    }

    // ON CONFLICT DO NOTHING fired — another row with the same
    // (slug, unique_key) is active. SELECT it. `unique_key` must be
    // Some here because that's the only path that triggers conflict.
    let key = opts.unique_key.unwrap_or_default();
    let existing = find_active_by_unique_key(conn, opts.slug, key)?.ok_or_else(|| {
        anyhow::anyhow!(
            "INSERT skipped via ON CONFLICT but no active row found for unique key '{key}' \
             on slug '{}' — likely a concurrent state change between conflict and lookup",
            opts.slug
        )
    })?;
    Ok(InsertedJob::Existing(existing))
}

/// Look up an active (pending or running) job for the given
/// `(slug, unique_key)` pair. Used by [`insert_job_with`] for
/// unique-key dedup fallback after `ON CONFLICT DO NOTHING`.
fn find_active_by_unique_key(
    conn: &dyn DbConnection,
    slug: &str,
    unique_key: &str,
) -> Result<Option<JobRun>> {
    let p1 = conn.placeholder(1);
    let p2 = conn.placeholder(2);
    let row = conn.query_one(
        &format!(
            "SELECT id FROM _crap_jobs \
             WHERE slug = {p1} AND unique_key = {p2} \
                AND status IN ('pending', 'running') \
             ORDER BY created_at ASC LIMIT 1"
        ),
        &[
            DbValue::Text(slug.to_string()),
            DbValue::Text(unique_key.to_string()),
        ],
    )?;

    let Some(row) = row else { return Ok(None) };
    let Some(id) = row.opt_text_at(0) else {
        return Ok(None);
    };

    // Round-trip through `get_job_run` to use the canonical SELECT
    // (with priority + other fields). The extra query is paid only
    // on the rare conflict path.
    get_job_run(conn, &id)
}

/// Mark a job as completed with an optional result.
///
/// # Errors
///
/// Returns a backend error if the UPDATE fails.
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
/// Formula: `min(2^(attempt-1) * 5, 300)` — yields 5s, 10s, 20s, 40s, 80s, 160s, 300s cap.
/// `attempt` is 1-based (first failure = attempt 1).
fn backoff_seconds(attempt: u32) -> i64 {
    let exp = i64::from(attempt.saturating_sub(1).min(6));

    cmp::min(5 * (1i64 << exp), 300)
}

/// Mark a job as failed. If `should_retry` is true, resets to pending with exponential backoff.
/// `attempt` is the current attempt number (already incremented by claim).
///
/// # Errors
///
/// Returns a backend error if the UPDATE fails.
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

        // retry_after = now + delay (a future time). A negative offset yields
        // a future timestamp under `date_offset_expr`'s `now - seconds`
        // contract; use its returned param so this works on every backend
        // (the previous hardcoded "+N seconds" text param was SQLite-only and
        // is a type error against Postgres `make_interval`).
        let (offset_sql, offset_param) = conn.date_offset_expr(-delay, 3);

        conn.execute(
            &format!(
                "UPDATE _crap_jobs SET status = 'pending', error = {p2}, \
                 started_at = NULL, completed_at = NULL, heartbeat_at = NULL, \
                 retry_after = {offset_sql} \
                 WHERE id = {p1}"
            ),
            &[
                DbValue::Text(id.to_string()),
                DbValue::Text(error.to_string()),
                offset_param,
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
///
/// # Errors
///
/// Returns a backend error if the UPDATE fails.
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

/// Mark a running job as stale.
///
/// # Errors
///
/// Returns a backend error if the UPDATE fails.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::JobStatus;
    use crate::db::query::jobs::get_job_run;
    use crate::db::query::jobs::test_helpers::setup_db;

    #[test]
    fn test_insert_and_get_job() {
        let (_dir, conn) = setup_db();
        let job = insert_job(&conn, "test_job", "{}", "manual", 1, "default", 0).unwrap();
        assert_eq!(job.slug, "test_job");
        assert_eq!(job.status, JobStatus::Pending);

        let fetched = get_job_run(&conn, &job.id).unwrap().unwrap();
        assert_eq!(fetched.slug, "test_job");
        assert_eq!(fetched.status, JobStatus::Pending);
    }

    #[test]
    fn test_complete_job() {
        let (_dir, conn) = setup_db();
        let job = insert_job(&conn, "test", "{}", "manual", 1, "default", 0).unwrap();
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
        let job = insert_job(&conn, "test", "{}", "manual", 1, "default", 0).unwrap();
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
        let job = insert_job(&conn, "test", "{}", "manual", 3, "default", 0).unwrap();
        conn.execute(
            "UPDATE _crap_jobs SET status = 'running', attempt = 1 WHERE id = ?1",
            &[DbValue::Text(job.id.clone())],
        )
        .unwrap();

        fail_job(&conn, &job.id, "transient error", true, 1).unwrap();
        let fetched = get_job_run(&conn, &job.id).unwrap().unwrap();
        assert_eq!(fetched.status, JobStatus::Pending);
    }

    /// Regression: `fail_job` with retry did not clear `heartbeat_at`, causing the
    /// re-queued job to be immediately detected as stale by `find_stale_jobs`.
    #[test]
    fn test_fail_job_retry_clears_heartbeat() {
        let (_dir, conn) = setup_db();
        let job = insert_job(&conn, "test", "{}", "manual", 3, "default", 0).unwrap();
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
    fn test_mark_stale() {
        let (_dir, conn) = setup_db();
        let job = insert_job(&conn, "test", "{}", "manual", 1, "default", 0).unwrap();
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
        let job = insert_job(&conn, "test", "{}", "manual", 1, "default", 0).unwrap();
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
    fn test_backoff_seconds() {
        // attempt is 1-based (first failure = 1 after claim increments)
        assert_eq!(backoff_seconds(0), 5); // edge case: 2^0 * 5 = 5
        assert_eq!(backoff_seconds(1), 5); // first failure: 2^0 * 5 = 5
        assert_eq!(backoff_seconds(2), 10); // second: 2^1 * 5 = 10
        assert_eq!(backoff_seconds(3), 20); // third: 2^2 * 5 = 20
        assert_eq!(backoff_seconds(4), 40);
        assert_eq!(backoff_seconds(5), 80);
        assert_eq!(backoff_seconds(6), 160);
        assert_eq!(backoff_seconds(7), 300); // capped
        // Capped at 300
        assert_eq!(backoff_seconds(8), 300);
        assert_eq!(backoff_seconds(100), 300);
    }

    /// Regression: `fail_job` with retry did not set `retry_after`, causing immediate re-execution.
    #[test]
    fn test_fail_job_retry_sets_retry_after() {
        let (_dir, conn) = setup_db();
        let job = insert_job(&conn, "test", "{}", "manual", 3, "default", 0).unwrap();
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

    // ── insert_job_with: delay ──────────────────────────────────────

    /// `delay_secs > 0` sets `retry_after` so the job isn't claimable
    /// until the delay elapses.
    #[test]
    fn insert_with_delay_sets_future_retry_after() {
        let (_dir, conn) = setup_db();
        let opts = InsertJobOpts {
            slug: "delayed",
            data: "{}",
            scheduled_by: "test",
            max_attempts: 1,
            queue: "default",
            priority: 0,
            delay_secs: 60,
            unique_key: None,
        };
        let inserted = insert_job_with(&conn, &opts).unwrap();
        let job = inserted.into_inner();

        // Round-trip through the DB so we see the persisted retry_after.
        let fetched = get_job_run(&conn, &job.id).unwrap().unwrap();
        assert!(
            fetched.retry_after.is_some(),
            "retry_after must be set when delay_secs > 0"
        );
    }

    /// `delay_secs == 0` leaves `retry_after` NULL — claimable
    /// immediately.
    #[test]
    fn insert_without_delay_leaves_retry_after_null() {
        let (_dir, conn) = setup_db();
        let opts = InsertJobOpts {
            slug: "immediate",
            data: "{}",
            scheduled_by: "test",
            max_attempts: 1,
            queue: "default",
            priority: 0,
            delay_secs: 0,
            unique_key: None,
        };
        let inserted = insert_job_with(&conn, &opts).unwrap();
        let job = inserted.into_inner();

        let fetched = get_job_run(&conn, &job.id).unwrap().unwrap();
        assert!(
            fetched.retry_after.is_none(),
            "retry_after must be NULL when delay_secs == 0"
        );
    }

    // ── insert_job_with: unique_key ─────────────────────────────────

    /// Second insert with same `(slug, unique_key)` returns the
    /// existing pending job — no duplicate row.
    #[test]
    fn insert_with_unique_key_dedups_pending() {
        let (_dir, conn) = setup_db();
        let opts = InsertJobOpts {
            slug: "cleanup",
            data: "{}",
            scheduled_by: "test",
            max_attempts: 1,
            queue: "default",
            priority: 0,
            delay_secs: 0,
            unique_key: Some("user:42"),
        };

        let first = insert_job_with(&conn, &opts).unwrap();
        let first_id = match &first {
            InsertedJob::Created(j) => j.id.clone(),
            InsertedJob::Existing(_) => panic!("first insert should be Created"),
        };

        let second = insert_job_with(&conn, &opts).unwrap();
        let second_id = match &second {
            InsertedJob::Existing(j) => j.id.clone(),
            InsertedJob::Created(_) => panic!("second insert should be Existing (dedup'd)"),
        };

        assert_eq!(first_id, second_id, "dedup must return the original job id");

        // Verify only one row in the DB.
        let row = conn
            .query_one(
                "SELECT COUNT(*) FROM _crap_jobs WHERE slug = 'cleanup' AND unique_key = 'user:42'",
                &[],
            )
            .unwrap()
            .unwrap();
        assert_eq!(row.i64_at(0), Some(1));
    }

    /// A `running` job (not just pending) still blocks unique-key
    /// re-enqueue. The partial unique index covers both states.
    #[test]
    fn insert_with_unique_key_dedups_against_running() {
        let (_dir, conn) = setup_db();
        let opts = InsertJobOpts {
            slug: "cleanup",
            data: "{}",
            scheduled_by: "test",
            max_attempts: 1,
            queue: "default",
            priority: 0,
            delay_secs: 0,
            unique_key: Some("user:99"),
        };

        let first = insert_job_with(&conn, &opts).unwrap().into_inner();
        // Promote the first to 'running' (worker claimed it) — still
        // active, so the next enqueue must dedup.
        conn.execute(
            "UPDATE _crap_jobs SET status = 'running' WHERE id = ?1",
            &[DbValue::Text(first.id.clone())],
        )
        .unwrap();

        let second = insert_job_with(&conn, &opts).unwrap();
        let second_id = match &second {
            InsertedJob::Existing(j) => j.id.clone(),
            InsertedJob::Created(_) => {
                panic!("running job must block unique-key re-enqueue")
            }
        };
        assert_eq!(second_id, first.id);
    }

    /// Completed/failed jobs don't block a new unique-key enqueue —
    /// only pending/running are considered active.
    #[test]
    fn insert_with_unique_key_allows_reuse_after_completion() {
        let (_dir, conn) = setup_db();
        let opts = InsertJobOpts {
            slug: "cleanup",
            data: "{}",
            scheduled_by: "test",
            max_attempts: 1,
            queue: "default",
            priority: 0,
            delay_secs: 0,
            unique_key: Some("user:42"),
        };

        let first = insert_job_with(&conn, &opts).unwrap().into_inner();
        // Mark the first as completed so it no longer blocks the unique key.
        conn.execute(
            "UPDATE _crap_jobs SET status = 'completed' WHERE id = ?1",
            &[DbValue::Text(first.id.clone())],
        )
        .unwrap();

        let second = insert_job_with(&conn, &opts).unwrap();
        match &second {
            InsertedJob::Created(j) => assert_ne!(j.id, first.id, "must be a new row"),
            InsertedJob::Existing(_) => {
                panic!("dedup should not match completed jobs")
            }
        }
    }

    /// `unique_key` is surfaced on the returned `JobRun` so callers
    /// reading back (admin, gRPC) can see what key the job was queued
    /// under.
    #[test]
    fn unique_key_surfaces_on_job_run() {
        let (_dir, conn) = setup_db();
        let opts = InsertJobOpts {
            slug: "cleanup",
            data: "{}",
            scheduled_by: "test",
            max_attempts: 1,
            queue: "default",
            priority: 0,
            delay_secs: 0,
            unique_key: Some("user:7"),
        };
        let inserted = insert_job_with(&conn, &opts).unwrap().into_inner();

        // Round-trip through the DB so we exercise the SELECT path
        // (the in-memory builder shortcut bypasses row_to_job_run).
        let fetched = get_job_run(&conn, &inserted.id).unwrap().unwrap();
        assert_eq!(fetched.unique_key.as_deref(), Some("user:7"));
    }

    /// `unique_key = None` allows arbitrary duplicate inserts.
    #[test]
    fn insert_without_unique_key_allows_duplicates() {
        let (_dir, conn) = setup_db();
        let opts = InsertJobOpts {
            slug: "noisy",
            data: "{}",
            scheduled_by: "test",
            max_attempts: 1,
            queue: "default",
            priority: 0,
            delay_secs: 0,
            unique_key: None,
        };

        let _a = insert_job_with(&conn, &opts).unwrap();
        let _b = insert_job_with(&conn, &opts).unwrap();
        let _c = insert_job_with(&conn, &opts).unwrap();

        let row = conn
            .query_one("SELECT COUNT(*) FROM _crap_jobs WHERE slug = 'noisy'", &[])
            .unwrap()
            .unwrap();
        assert_eq!(row.i64_at(0), Some(3));
    }
}
