//! Cron schedule evaluation: insert pending jobs for due schedules.

use std::collections::HashMap;

use anyhow::{Context as _, Result};
use chrono::{DateTime, Utc};
use cron::Schedule;
use tracing::{debug, info, warn};

use crate::{
    core::{JobDefinition, Registry},
    db::{DbConnection, DbPool, query::jobs as job_query},
    scheduler::runner::cron_expr::parse_cron,
};

/// One schedule's slot in a single cron tick.
struct CronTick<'a> {
    slug: &'a str,
    def: &'a JobDefinition,
    /// When this process last evaluated cron. Only used for a slug that has
    /// never fired — everything else resumes from the persisted window.
    last_check: DateTime<Utc>,
    now: DateTime<Utc>,
    queue_retries: &'a HashMap<String, u32>,
}

/// Parse the definition's cron expression, or `None` when there is none or it
/// is unusable.
///
/// Every schedule is parsed at startup too, so an unusable expression here
/// means the definition changed under a running process.
fn parse_schedule(tick: &CronTick<'_>) -> Option<Schedule> {
    let schedule_str = tick.def.schedule.as_ref()?;

    parse_cron(schedule_str)
        .inspect_err(|e| {
            warn!(
                "Invalid cron expression '{}' for job '{}': {}",
                schedule_str, tick.slug, e
            );
        })
        .ok()
}

/// Where this slug's window starts: the persisted last fire when there is
/// one, the process's own last check only when the slug has never fired.
///
/// Anchoring on the persisted value is what lets a schedule that came due
/// while the process was down still fire — a process-local start is seeded at
/// boot, so the whole downtime falls outside every window that follows.
///
/// The stored string is handed back untouched. The claim compares it against
/// `fired_at` as text, and the boundary case where the two are equal is
/// load-bearing, so re-rendering it here could shift the comparison.
fn window_start(conn: &dyn DbConnection, tick: &CronTick<'_>) -> Result<(DateTime<Utc>, String)> {
    let Some(stored) = job_query::cron_fired_at(conn, tick.slug)? else {
        return Ok((tick.last_check, tick.last_check.to_rfc3339()));
    };

    let Ok(parsed) = DateTime::parse_from_rfc3339(&stored) else {
        warn!(
            "Unreadable last fire time '{}' for job '{}' — using this process's window instead",
            stored, tick.slug
        );

        return Ok((tick.last_check, tick.last_check.to_rfc3339()));
    };

    Ok((parsed.with_timezone(&Utc), stored))
}

/// Insert the pending run.
///
/// `effective_max_attempts` resolves `JobDefinition.retries` first, falling
/// back to `[jobs.queues.<queue>] retries` when the definition didn't set it.
fn insert_cron_run(conn: &dyn DbConnection, tick: &CronTick<'_>) -> Result<()> {
    let def = tick.def;

    let job = job_query::insert_job(
        conn,
        tick.slug,
        "{}",
        "cron",
        def.effective_max_attempts(tick.queue_retries.get(&def.queue).copied()),
        &def.queue,
        def.priority,
    )?;

    info!("Cron scheduled job '{}' (run {})", tick.slug, job.id);

    Ok(())
}

/// Queue one schedule's run if it came due in this window and this instance
/// wins the window.
fn fire_if_due(conn: &dyn DbConnection, tick: &CronTick<'_>) -> Result<()> {
    let Some(schedule) = parse_schedule(tick) else {
        return Ok(());
    };

    let (start, start_text) = window_start(conn, tick)?;

    // One run however long the gap: taking only the first due time caps a
    // long downtime to a single catch-up rather than a burst of missed runs.
    let should_fire = schedule
        .after(&start)
        .take_while(|t| *t <= tick.now)
        .next()
        .is_some();

    if !should_fire {
        return Ok(());
    }

    // Atomic cron dedup: only one instance wins each cron window.
    // Uses _crap_cron_fired table to prevent double-fire in multi-server.
    let fired_at = tick.now.to_rfc3339();

    if !job_query::try_claim_cron_window(conn, tick.slug, &fired_at, &start_text)? {
        debug!(
            "Cron job '{}' already fired by another instance in this window",
            tick.slug
        );

        return Ok(());
    }

    // skip_if_running is atomic with the insert inside the same IMMEDIATE
    // transaction.
    if tick.def.skip_if_running && job_query::count_running(conn, Some(tick.slug))? > 0 {
        debug!("Skipping cron job '{}' — still running", tick.slug);

        return Ok(());
    }

    insert_cron_run(conn, tick)
}

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
    let mut conn = pool
        .write()
        .context("Failed to get DB connection for cron")?;
    let tx = conn
        .transaction_immediate()
        .context("Failed to start cron check transaction")?;

    for (slug, def) in &registry.jobs {
        fire_if_due(
            &tx,
            &CronTick {
                slug,
                def,
                last_check,
                now,
                queue_retries,
            },
        )?;
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
        db::{DbConnection, DbValue},
        scheduler::runner::test_support::{make_registry_with_jobs, make_test_pool},
    };

    /// Persist a last-fire time for `slug`, standing in for a run that
    /// happened before this process started.
    fn record_fire(pool: &DbPool, slug: &str, at: DateTime<Utc>) {
        let conn = pool.get().unwrap();

        conn.execute(
            "INSERT INTO _crap_cron_fired (slug, fired_at) VALUES (?1, ?2)",
            &[
                DbValue::Text(slug.to_string()),
                DbValue::Text(at.to_rfc3339()),
            ],
        )
        .unwrap();
    }

    /// Pending + finished runs recorded for `slug`.
    fn runs_for(pool: &DbPool, slug: &str) -> usize {
        let conn = pool.get().unwrap();

        job_query::list_job_runs(&conn, Some(slug), None, 100, 0)
            .unwrap()
            .len()
    }

    /// Regression: the scheduler seeds its in-process window at start, so a
    /// schedule that came due while the process was down fell outside every
    /// window that followed and never fired at all. The persisted fire time
    /// is the anchor now, so the first tick after a restart catches it up —
    /// once, however long the gap.
    #[test]
    fn a_schedule_due_during_downtime_fires_once_after_the_restart() {
        let pool = make_test_pool();
        let registry = make_registry_with_jobs(vec![
            JobDefinition::builder("downtime_job", "some.handler")
                .schedule("* * * * *")
                .skip_if_running(false)
                .build(),
        ]);

        let now = Utc::now();
        record_fire(&pool, "downtime_job", now - Duration::hours(2));

        // A freshly started process: its own window is empty.
        check_cron_schedules(&pool, &registry, now, now, &HashMap::new()).unwrap();
        assert_eq!(
            runs_for(&pool, "downtime_job"),
            1,
            "a window missed during downtime must be caught up"
        );

        // The catch-up recorded its own fire, so the next tick is quiet —
        // two hours of missed minutes do not become two hours of runs.
        check_cron_schedules(&pool, &registry, now, now, &HashMap::new()).unwrap();
        assert_eq!(
            runs_for(&pool, "downtime_job"),
            1,
            "the catch-up must fire exactly once"
        );
    }

    /// The persisted fire time also wins in the other direction: a job that
    /// already fired inside the current window must not fire again just
    /// because this process's own last check is hours old.
    #[test]
    fn a_recent_recorded_fire_beats_a_stale_process_window() {
        let pool = make_test_pool();
        let registry = make_registry_with_jobs(vec![
            JobDefinition::builder("hourly_job", "some.handler")
                .schedule("0 * * * *") // every hour at :00
                .build(),
        ]);

        // :30:30, with the last fire one second earlier — no hour boundary
        // in between, so nothing is due.
        let now = Utc::now().with_minute(30).unwrap().with_second(30).unwrap();
        record_fire(&pool, "hourly_job", now - Duration::seconds(1));

        check_cron_schedules(
            &pool,
            &registry,
            now - Duration::hours(2),
            now,
            &HashMap::new(),
        )
        .unwrap();

        assert_eq!(
            runs_for(&pool, "hourly_job"),
            0,
            "a stale process window must not re-fire a schedule that just fired"
        );
    }

    /// With nothing recorded there is no durable window to resume from, so
    /// the process's own last check is the anchor.
    #[test]
    fn a_never_fired_slug_falls_back_to_the_process_window() {
        let pool = make_test_pool();
        let registry = make_registry_with_jobs(vec![
            JobDefinition::builder("fresh_job", "some.handler")
                .schedule("* * * * *")
                .skip_if_running(false)
                .build(),
        ]);

        let now = Utc::now();

        // Empty process window, nothing recorded: nothing to fire.
        check_cron_schedules(&pool, &registry, now, now, &HashMap::new()).unwrap();
        assert_eq!(runs_for(&pool, "fresh_job"), 0);

        // A window that spans a due minute fires it.
        check_cron_schedules(
            &pool,
            &registry,
            now - Duration::minutes(2),
            now,
            &HashMap::new(),
        )
        .unwrap();
        assert_eq!(runs_for(&pool, "fresh_job"), 1);
    }

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
