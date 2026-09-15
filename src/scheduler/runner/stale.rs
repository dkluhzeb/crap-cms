//! Stale job recovery: requeue or terminate jobs whose worker died.

use anyhow::Result;
use tracing::info;

use crate::{
    core::{Registry, job::SYSTEM_BULK_JOB},
    db::{DbConnection, query::jobs as job_query},
    scheduler::bulk::strip_finished_payload,
};

/// Recover jobs whose owning worker died mid-execution (heartbeat expired).
///
/// A `running` row whose `heartbeat_at` is older than `stale_threshold_secs`
/// (or null) is assumed dead — its worker stopped heartbeating. This is the
/// at-least-once delivery guarantee: a retryable dead job is **requeued** (so a
/// surviving peer re-runs it) and an exhausted one is marked terminal `stale`.
///
/// Runs both at startup (recover this node's own pre-crash jobs) and
/// periodically at runtime (any node reclaims a crashed peer's jobs). The
/// threshold MUST exceed the heartbeat interval so a merely-slow heartbeat
/// doesn't wrongly reclaim a live job; the caller passes
/// `heartbeat_interval * N`. All writes are compare-and-set on
/// `(running, attempt)` so two nodes recovering the same job — or the original
/// worker briefly resuming — cannot double-act or clobber the result.
///
/// # Errors
///
/// Returns an error if listing stale jobs or marking any one stale fails.
pub fn recover_stale_jobs(
    conn: &dyn DbConnection,
    registry: &Registry,
    stale_threshold_secs: u64,
) -> Result<()> {
    let stale = job_query::find_stale_jobs(conn, stale_threshold_secs)?;

    let mut requeued = 0u32;
    let mut terminal = 0u32;

    for job in &stale {
        let _ = registry.jobs.get(job.slug.as_str()); // slug may be undefined; still recover

        if job.attempt < job.max_attempts {
            // Retryable → requeue with backoff (guarded on running+attempt).
            job_query::fail_job(
                conn,
                &job.id,
                "stale: worker heartbeat expired, requeued",
                true,
                job.attempt,
            )?;
            requeued += 1;
            info!(
                "Requeued stale job {} ({}) attempt {}/{}",
                job.id, job.slug, job.attempt, job.max_attempts
            );
        } else {
            // Retries exhausted → terminal stale (guarded).
            job_query::mark_stale(
                conn,
                &job.id,
                job.attempt,
                "stale: worker heartbeat expired, retries exhausted",
            )?;

            // `stale` is terminal: a bulk run drops its request payload here too.
            if job.slug == SYSTEM_BULK_JOB {
                strip_finished_payload(conn, job);
            }

            terminal += 1;
            info!("Marked stale job {} ({})", job.id, job.slug);
        }
    }

    if requeued > 0 || terminal > 0 {
        info!("Recovered stale jobs: {requeued} requeued, {terminal} terminal");
    }

    Ok(())
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use super::*;
    use crate::{
        core::{JobDefinition, job::JobStatus},
        scheduler::runner::test_support::{make_registry_with_jobs, make_test_pool},
    };

    const TEST_STALE_THRESHOLD: u64 = 30;

    /// At-least-once: a dead (stale-heartbeat) job that still has retries left
    /// is REQUEUED (→ pending), so a surviving peer re-runs it.
    #[test]
    fn recover_requeues_retryable_stale_job() {
        let pool = make_test_pool();
        let conn = pool.get().unwrap();
        let registry = make_registry_with_jobs(vec![
            JobDefinition::builder("my_job", "some.handler").build(),
        ]);

        // Running at attempt 1 of 3, heartbeat 600s stale (worker died).
        job_query::insert_job(&conn, "my_job", "{}", "manual", 3, "default", 0).unwrap();
        conn.execute_batch(
            "UPDATE _crap_jobs SET status = 'running', attempt = 1, \
             heartbeat_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-600 seconds')",
        )
        .unwrap();

        recover_stale_jobs(&conn, &registry, TEST_STALE_THRESHOLD).unwrap();

        let pending =
            job_query::list_job_runs(&conn, None, Some(JobStatus::Pending), 100, 0).unwrap();
        assert_eq!(pending.len(), 1, "retryable stale job must be requeued");
        assert_eq!(
            job_query::list_job_runs(&conn, None, Some(JobStatus::Stale), 100, 0)
                .unwrap()
                .len(),
            0
        );
    }

    /// A dead job that has exhausted its retries goes terminal `stale`.
    #[test]
    fn recover_marks_exhausted_stale_job_terminal() {
        let pool = make_test_pool();
        let conn = pool.get().unwrap();
        let registry = make_registry_with_jobs(vec![
            JobDefinition::builder("my_job", "some.handler").build(),
        ]);

        // Running at attempt 1 of 1 (no retries left).
        job_query::insert_job(&conn, "my_job", "{}", "manual", 1, "default", 0).unwrap();
        conn.execute_batch(
            "UPDATE _crap_jobs SET status = 'running', attempt = 1, \
             heartbeat_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-600 seconds')",
        )
        .unwrap();

        recover_stale_jobs(&conn, &registry, TEST_STALE_THRESHOLD).unwrap();

        let stale = job_query::list_job_runs(&conn, None, Some(JobStatus::Stale), 100, 0).unwrap();
        assert_eq!(stale.len(), 1);
        assert!(
            stale[0]
                .error
                .as_ref()
                .unwrap()
                .contains("heartbeat expired")
        );
    }

    /// Regression: a bulk run recovered as terminal `stale` kept its submitted
    /// payload at rest; it now keeps only the identity its visibility reads.
    #[test]
    fn recover_strips_the_payload_of_a_stale_bulk_run() {
        let pool = make_test_pool();
        let conn = pool.get().unwrap();
        let registry = make_registry_with_jobs(Vec::new());

        let data = r#"{"op":"create_many","collection":"posts","queued_by":{"kind":"system"},"max_documents":10,"documents":[{"title":"secret-ish"}]}"#;
        job_query::insert_job(&conn, SYSTEM_BULK_JOB, data, "grpc", 1, "bulk", 0).unwrap();
        conn.execute_batch(
            "UPDATE _crap_jobs SET status = 'running', attempt = 1, \
             heartbeat_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-600 seconds')",
        )
        .unwrap();

        recover_stale_jobs(&conn, &registry, TEST_STALE_THRESHOLD).unwrap();

        let stale = job_query::list_job_runs(&conn, None, Some(JobStatus::Stale), 100, 0).unwrap();
        assert_eq!(stale.len(), 1);
        assert!(!stale[0].data.contains("secret-ish"), "{}", stale[0].data);
        assert!(
            stale[0].data.contains("queued_by") && stale[0].data.contains("posts"),
            "{}",
            stale[0].data
        );
    }

    /// The multi-node fix: a running job with a FRESH heartbeat belongs to a
    /// live peer and must NOT be recovered.
    #[test]
    fn recover_ignores_fresh_heartbeat_job() {
        let pool = make_test_pool();
        let conn = pool.get().unwrap();
        let registry = make_registry_with_jobs(vec![
            JobDefinition::builder("my_job", "some.handler").build(),
        ]);

        job_query::insert_job(&conn, "my_job", "{}", "manual", 3, "default", 0).unwrap();
        conn.execute_batch(
            "UPDATE _crap_jobs SET status = 'running', attempt = 1, heartbeat_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
        )
        .unwrap();

        recover_stale_jobs(&conn, &registry, TEST_STALE_THRESHOLD).unwrap();

        // Untouched: still running, not requeued, not stale.
        assert_eq!(
            job_query::list_job_runs(&conn, None, Some(JobStatus::Running), 100, 0)
                .unwrap()
                .len(),
            1,
            "a live peer's fresh-heartbeat job must not be reclaimed"
        );
    }

    #[test]
    fn recover_ignores_pending_job() {
        let pool = make_test_pool();
        let conn = pool.get().unwrap();
        let registry = make_registry_with_jobs(vec![]);

        job_query::insert_job(&conn, "my_job", "{}", "manual", 1, "default", 0).unwrap();

        recover_stale_jobs(&conn, &registry, TEST_STALE_THRESHOLD).unwrap();

        assert_eq!(
            job_query::list_job_runs(&conn, None, Some(JobStatus::Stale), 100, 0)
                .unwrap()
                .len(),
            0
        );
        assert_eq!(
            job_query::list_job_runs(&conn, None, Some(JobStatus::Pending), 100, 0)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn recover_multiple_running() {
        let pool = make_test_pool();
        let conn = pool.get().unwrap();
        let registry = make_registry_with_jobs(vec![
            JobDefinition::builder("job_a", "handler_a").build(),
            JobDefinition::builder("job_b", "handler_b").build(),
        ]);

        // Both retryable + stale (null heartbeat) → both requeued.
        job_query::insert_job(&conn, "job_a", "{}", "manual", 3, "default", 0).unwrap();
        job_query::insert_job(&conn, "job_b", "{}", "manual", 3, "default", 0).unwrap();
        conn.execute_batch("UPDATE _crap_jobs SET status = 'running', attempt = 1")
            .unwrap();

        recover_stale_jobs(&conn, &registry, TEST_STALE_THRESHOLD).unwrap();

        assert_eq!(
            job_query::list_job_runs(&conn, None, Some(JobStatus::Pending), 100, 0)
                .unwrap()
                .len(),
            2
        );
    }
}
