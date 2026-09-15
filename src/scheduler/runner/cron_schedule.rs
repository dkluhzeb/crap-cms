//! Cron schedule evaluation: insert pending jobs for due schedules.

use std::{collections::HashMap, str::FromStr};

use anyhow::{Context as _, Result};
use chrono::{DateTime, Utc};
use cron::Schedule;
use tracing::{debug, info, warn};

use crate::{
    core::Registry,
    db::{DbPool, query::jobs as job_query},
    scheduler::runner::cron_expr::normalize_cron,
};

/// Check cron schedules and insert pending jobs for due ones.
///
/// # Errors
///
/// Returns an error if the connection, transaction, or job insertion fails.
pub fn check_cron_schedules(
    pool: &DbPool,
    registry: &Registry,
    last_check: DateTime<Utc>,
    now: DateTime<Utc>,
    queue_retries: &HashMap<String, u32>,
) -> Result<()> {
    let mut conn = pool.get().context("Failed to get DB connection for cron")?;
    let tx = conn
        .transaction_immediate()
        .context("Failed to start cron check transaction")?;

    for (slug, def) in &registry.jobs {
        let Some(schedule_str) = &def.schedule else {
            continue;
        };

        // Parse cron expression (the cron crate expects 6-7 fields with seconds;
        // normalize standard 5-field expressions by prepending "0" for seconds)
        let normalized = normalize_cron(schedule_str);
        let schedule = match Schedule::from_str(&normalized) {
            Ok(s) => s,
            Err(e) => {
                warn!(
                    "Invalid cron expression '{}' for job '{}': {}",
                    schedule_str, slug, e
                );

                continue;
            }
        };

        // Check if the schedule should have fired between last_check and now
        let should_fire = schedule
            .after(&last_check)
            .take_while(|t| *t <= now)
            .next()
            .is_some();

        if !should_fire {
            continue;
        }

        // Atomic cron dedup: only one instance wins each cron window.
        // Uses _crap_cron_fired table to prevent double-fire in multi-server.
        let fired_at = now.to_rfc3339();
        let window_start = last_check.to_rfc3339();

        if !job_query::try_claim_cron_window(&tx, slug, &fired_at, &window_start)? {
            debug!(
                "Cron job '{}' already fired by another instance in this window",
                slug
            );

            continue;
        }

        // Check skip_if_running (atomic with insert inside the same IMMEDIATE transaction)
        if def.skip_if_running {
            let running = job_query::count_running(&tx, Some(slug))?;

            if running > 0 {
                debug!("Skipping cron job '{}' — still running", slug);

                continue;
            }
        }

        // Insert a pending job. `effective_max_attempts` resolves
        // `JobDefinition.retries` first, falling back to
        // `[jobs.queues.<queue>] retries` when the definition didn't
        // set it.
        let job = job_query::insert_job(
            &tx,
            slug,
            "{}",
            "cron",
            def.effective_max_attempts(queue_retries.get(&def.queue).copied()),
            &def.queue,
            def.priority,
        )?;

        info!("Cron scheduled job '{}' (run {})", slug, job.id);
    }

    tx.commit()
        .context("Failed to commit cron check transaction")?;

    Ok(())
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use chrono::{Duration, Timelike};

    use super::*;
    use crate::{
        core::{JobDefinition, job::JobStatus},
        db::DbConnection,
        scheduler::runner::test_support::{make_registry_with_jobs, make_test_pool},
    };

    #[test]
    fn check_cron_schedules_fires_due_job() {
        let pool = make_test_pool();
        let registry = make_registry_with_jobs(vec![
            JobDefinition::builder("cron_job", "some.handler")
                .schedule("* * * * *") // every minute
                .retries(0)
                .queue("default")
                .skip_if_running(false)
                .build(),
        ]);

        // Set last_check to 2 minutes ago, now to current — schedule should fire
        let now = Utc::now();
        let last_check = now - Duration::minutes(2);

        check_cron_schedules(&pool, &registry, last_check, now, &HashMap::new()).unwrap();

        let conn = pool.get().unwrap();
        let jobs = job_query::list_job_runs(&conn, Some("cron_job"), None, 100, 0).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].status, JobStatus::Pending);
        assert_eq!(jobs[0].scheduled_by.as_deref(), Some("cron"));
    }

    #[test]
    fn check_cron_schedules_skips_no_schedule_jobs() {
        let pool = make_test_pool();
        let registry = make_registry_with_jobs(vec![
            JobDefinition::builder("no_cron_job", "some.handler").build(), // no schedule
        ]);

        let now = Utc::now();
        let last_check = now - Duration::minutes(2);

        check_cron_schedules(&pool, &registry, last_check, now, &HashMap::new()).unwrap();

        let conn = pool.get().unwrap();
        let jobs = job_query::list_job_runs(&conn, None, None, 100, 0).unwrap();
        assert_eq!(jobs.len(), 0);
    }

    #[test]
    fn check_cron_schedules_skips_not_due() {
        let pool = make_test_pool();
        let registry = make_registry_with_jobs(vec![
            JobDefinition::builder("hourly_job", "some.handler")
                .schedule("0 * * * *") // every hour at :00
                .build(),
        ]);

        // Use a fixed window that is guaranteed to NOT cross an hour boundary:
        // pick a time at minute :30 with a 1-second window.
        let now = Utc::now().with_minute(30).unwrap().with_second(30).unwrap();
        let last_check = now - Duration::seconds(1);

        check_cron_schedules(&pool, &registry, last_check, now, &HashMap::new()).unwrap();

        let conn = pool.get().unwrap();
        let jobs = job_query::list_job_runs(&conn, None, None, 100, 0).unwrap();
        assert_eq!(
            jobs.len(),
            0,
            "hourly job should not fire in a 1s window at :30"
        );
    }

    #[test]
    fn check_cron_schedules_skip_if_running() {
        let pool = make_test_pool();
        let registry = make_registry_with_jobs(vec![
            JobDefinition::builder("skip_job", "some.handler")
                .schedule("* * * * *")
                .skip_if_running(true)
                .build(),
        ]);

        // Insert a running job for this slug
        {
            let conn = pool.get().unwrap();
            job_query::insert_job(&conn, "skip_job", "{}", "manual", 1, "default", 0).unwrap();
            conn.execute_batch("UPDATE _crap_jobs SET status = 'running'")
                .unwrap();
        }

        let now = Utc::now();
        let last_check = now - Duration::minutes(2);

        check_cron_schedules(&pool, &registry, last_check, now, &HashMap::new()).unwrap();

        // Should NOT insert a new pending job because skip_if_running=true and one is running
        let conn = pool.get().unwrap();
        let pending =
            job_query::list_job_runs(&conn, Some("skip_job"), Some(JobStatus::Pending), 100, 0)
                .unwrap();
        assert_eq!(pending.len(), 0);
    }

    #[test]
    fn check_cron_schedules_no_skip_if_running_false() {
        let pool = make_test_pool();
        let registry = make_registry_with_jobs(vec![
            JobDefinition::builder("noskip_job", "some.handler")
                .schedule("* * * * *")
                .skip_if_running(false)
                .build(),
        ]);

        // Insert a running job
        {
            let conn = pool.get().unwrap();
            job_query::insert_job(&conn, "noskip_job", "{}", "manual", 1, "default", 0).unwrap();
            conn.execute_batch("UPDATE _crap_jobs SET status = 'running'")
                .unwrap();
        }

        let now = Utc::now();
        let last_check = now - Duration::minutes(2);

        check_cron_schedules(&pool, &registry, last_check, now, &HashMap::new()).unwrap();

        // Should insert a new pending job even though one is running
        let conn = pool.get().unwrap();
        let pending =
            job_query::list_job_runs(&conn, Some("noskip_job"), Some(JobStatus::Pending), 100, 0)
                .unwrap();
        assert_eq!(pending.len(), 1);
    }

    #[test]
    fn check_cron_schedules_invalid_cron_expression() {
        let pool = make_test_pool();
        let registry = make_registry_with_jobs(vec![
            JobDefinition::builder("bad_cron", "some.handler")
                .schedule("not a valid cron")
                .build(),
        ]);

        let now = Utc::now();
        let last_check = now - Duration::minutes(2);

        // Should not error, just skip the invalid expression
        check_cron_schedules(&pool, &registry, last_check, now, &HashMap::new()).unwrap();

        let conn = pool.get().unwrap();
        let jobs = job_query::list_job_runs(&conn, None, None, 100, 0).unwrap();
        assert_eq!(jobs.len(), 0);
    }

    /// Regression: a job defined without `retries` inherits
    /// `[jobs.queues.<queue>] retries` at cron-fire time. Pre-Option<u32>
    /// migration this silently collapsed to `0` (one attempt); the
    /// queue config wins now.
    #[test]
    fn check_cron_schedules_inherits_queue_retries() {
        let pool = make_test_pool();
        let registry = make_registry_with_jobs(vec![
            JobDefinition::builder("inherits_cron", "some.handler")
                .schedule("* * * * *")
                // NO .retries() call — JobDefinition.retries = None,
                // so the queue's `retries = 5` should apply.
                .queue("reports")
                .skip_if_running(false)
                .build(),
        ]);

        let mut queue_retries = HashMap::new();
        queue_retries.insert("reports".to_string(), 5);

        let now = Utc::now();
        let last_check = now - Duration::minutes(2);

        check_cron_schedules(&pool, &registry, last_check, now, &queue_retries).unwrap();

        let conn = pool.get().unwrap();
        let jobs = job_query::list_job_runs(&conn, Some("inherits_cron"), None, 100, 0).unwrap();
        assert_eq!(jobs.len(), 1);
        // queue retries=5 → max_attempts = 5 + 1 = 6 (inherited)
        assert_eq!(
            jobs[0].max_attempts, 6,
            "JobDefinition without retries should inherit [jobs.queues.reports] retries = 5"
        );
    }

    /// Companion to `check_cron_schedules_inherits_queue_retries`:
    /// explicit `.retries(0)` BEATS the queue default (operator chose
    /// no retries even though the queue says 5).
    #[test]
    fn check_cron_schedules_explicit_zero_retries_overrides_queue() {
        let pool = make_test_pool();
        let registry = make_registry_with_jobs(vec![
            JobDefinition::builder("explicit_zero_cron", "some.handler")
                .schedule("* * * * *")
                .retries(0) // explicit "no retries"
                .queue("reports")
                .skip_if_running(false)
                .build(),
        ]);

        let mut queue_retries = HashMap::new();
        queue_retries.insert("reports".to_string(), 5);

        let now = Utc::now();
        let last_check = now - Duration::minutes(2);

        check_cron_schedules(&pool, &registry, last_check, now, &queue_retries).unwrap();

        let conn = pool.get().unwrap();
        let jobs =
            job_query::list_job_runs(&conn, Some("explicit_zero_cron"), None, 100, 0).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(
            jobs[0].max_attempts, 1,
            "explicit retries(0) must override the queue default of 5"
        );
    }

    #[test]
    fn check_cron_schedules_retries_stored() {
        let pool = make_test_pool();
        let registry = make_registry_with_jobs(vec![
            JobDefinition::builder("retried_cron", "some.handler")
                .schedule("* * * * *")
                .retries(3)
                .queue("special")
                .skip_if_running(false)
                .build(),
        ]);

        let now = Utc::now();
        let last_check = now - Duration::minutes(2);

        check_cron_schedules(&pool, &registry, last_check, now, &HashMap::new()).unwrap();

        let conn = pool.get().unwrap();
        let jobs = job_query::list_job_runs(&conn, Some("retried_cron"), None, 100, 0).unwrap();
        assert_eq!(jobs.len(), 1);
        // retries=3 => max_attempts = retries + 1 = 4
        assert_eq!(jobs[0].max_attempts, 4);
        assert_eq!(jobs[0].queue, "special");
    }
}
