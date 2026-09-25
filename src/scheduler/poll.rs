//! The poll tick: claim runnable jobs up to the free capacity, execute each
//! claimed run on the blocking pool, and watch it until it ends.
//!
//! Two rules shape this module:
//!
//! - **A run is never abandoned while it still executes.** Tokio cannot
//!   cancel a blocking task, so a timer that gave up on one would leave its
//!   handler running — and committing — while the row went back to `pending`
//!   and a retry started next to it, outside every concurrency cap. A run
//!   therefore stops itself at its timeout (Lua handlers and `_system_bulk`
//!   enforce a cooperative deadline), and the scheduler's timer is a
//!   watchdog: it reports a run that is overdue and keeps waiting. The row
//!   stays `running` — heartbeat fresh, concurrency slot held — until the
//!   run has actually returned and recorded its own outcome.
//! - **A finished run frees its slot immediately.** Every completion wakes
//!   the loop, which polls again at once instead of waiting for the next
//!   poll tick; the tick stays as the fallback.

use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use anyhow::{Context as _, Result};
use tokio::{
    sync::Notify,
    task::{JoinError, JoinHandle, spawn_blocking},
    time::{Duration, timeout},
};
use tokio_util::task::TaskTracker;
use tracing::{error, warn};

use crate::{
    config::{
        DEFAULT_BULK_QUEUE_TIMEOUT_SECS, DEFAULT_EMAIL_QUEUE_TIMEOUT_SECS,
        DEFAULT_IMAGES_QUEUE_TIMEOUT_SECS, SELF_LIMITING_JOB_GRACE_SECS,
    },
    core::{
        JobDefinition, JobRun, Registry, SharedEmailProvider, SharedStorage,
        email::{SYSTEM_EMAIL_JOB, SYSTEM_EMAIL_QUEUE},
        job::{SYSTEM_BULK_JOB, SYSTEM_BULK_QUEUE},
        upload::{IMAGE_CONVERT_QUEUE, SYSTEM_IMAGE_CONVERT_JOB},
    },
    db::{
        BoxedConnection, DbPool,
        query::jobs::{self as job_query, ClaimParams},
    },
    hooks::{HookRunner, LuaCrudInfra},
    service::AppInfra,
};

use super::{
    bulk::strip_finished_payload,
    runner::{ExecuteJobParams, execute_job},
    types::{RunningJob, RunningJobs, TickJobConfig},
};

/// Everything a poll needs. Built once when the loop starts — every part is
/// immutable configuration or a shared handle — and shared by every poll.
pub(super) struct PollInput {
    pub pool: DbPool,
    pub hook_runner: HookRunner,
    pub registry: Arc<Registry>,
    pub max_concurrent: usize,
    pub running_jobs: RunningJobs,
    pub email_provider: Option<SharedEmailProvider>,
    /// Woken whenever a run finishes, so the loop polls again right away.
    pub run_finished: Arc<Notify>,
    pub system: TickJobConfig,
}

/// Single-flight gate for the poll that never loses a request.
///
/// Two overlapping polls would each read the same stale running count before
/// either claim commits and together claim past `max_concurrent`, so only
/// one poll runs at a time. A request that arrives while one is in flight is
/// not dropped: the in-flight poll runs one more pass for it. That matters
/// for the completion wake-up — a run finishing while a poll is still
/// spawning would otherwise leave its freed slot idle until the next tick.
#[derive(Default)]
pub(super) struct PollGate {
    in_flight: AtomicBool,
    again: AtomicBool,
}

impl PollGate {
    /// An idle gate.
    pub(super) fn new() -> Self {
        Self::default()
    }

    /// Ask for a poll. `true`: nothing is in flight and the caller starts
    /// one. `false`: the poll already in flight runs another pass for it.
    fn request(&self) -> bool {
        self.again.store(true, Ordering::SeqCst);

        !self.in_flight.swap(true, Ordering::SeqCst)
    }

    /// Start a pass. It covers every request made up to this point.
    fn begin_pass(&self) {
        self.again.store(false, Ordering::SeqCst);
    }

    /// After a pass: `true` when another is due (a request arrived during
    /// this one), `false` once the gate is released.
    fn finish_pass(&self) -> bool {
        if self.again.load(Ordering::SeqCst) {
            return true;
        }

        self.in_flight.store(false, Ordering::SeqCst);

        // A request landing between the check above and the release saw the
        // poll still in flight and left its pass to it: take the gate back
        // and serve it, unless a new poll already did.
        self.again.load(Ordering::SeqCst) && !self.in_flight.swap(true, Ordering::SeqCst)
    }

    /// Release the gate after a pass panicked, so polling resumes.
    fn release(&self) {
        self.in_flight.store(false, Ordering::SeqCst);
    }
}

/// Request a poll through `gate`. The passes run on the blocking pool,
/// tracked like the jobs they spawn: a poll caught mid-claim by the shutdown
/// must finish handing its claimed runs to the tracker before the drain
/// decides what to wait for.
pub(super) fn spawn_poll(tasks: &TaskTracker, gate: &Arc<PollGate>, input: &Arc<PollInput>) {
    let input = Arc::clone(input);

    spawn_gated(tasks, gate, move || {
        if let Err(e) = poll_and_execute(&input) {
            error!("Scheduler poll error: {}", e);
        }
    });
}

/// Run `pass` through `gate` — once per request, never two at a time.
fn spawn_gated<F>(tasks: &TaskTracker, gate: &Arc<PollGate>, pass: F)
where
    F: Fn() + Send + 'static,
{
    if !gate.request() {
        return;
    }

    let gate = Arc::clone(gate);

    tasks.spawn(async move {
        let passes = Arc::clone(&gate);

        if let Err(e) = spawn_blocking(move || run_passes(&passes, &pass)).await {
            error!("Scheduler poll tick panicked: {e}");

            gate.release();
        }
    });
}

/// Run passes until no request is left.
fn run_passes(gate: &PollGate, pass: &dyn Fn()) {
    loop {
        gate.begin_pass();

        pass();

        if !gate.finish_pass() {
            break;
        }
    }
}

/// Claim pending jobs up to the free capacity and start each claimed run.
#[cfg(not(tarpaulin_include))]
fn poll_and_execute(p: &PollInput) -> Result<()> {
    // The write pool: `claim_pending_jobs` below opens an IMMEDIATE transaction
    // on this connection, and a write transaction on a read connection starves
    // concurrent readers.
    let mut conn = p.pool.write().context("Failed to get DB connection")?;

    let total_running = job_query::count_running(&conn, None)?;
    // Saturate to max_concurrent so a runaway counter still gates new jobs
    // (zero `available` = skip this tick rather than over-claiming).
    let running = usize::try_from(total_running).unwrap_or(p.max_concurrent);
    if running >= p.max_concurrent {
        return Ok(());
    }

    let job_concurrency = read_job_concurrency(&p.registry);
    let params = ClaimParams::all_queues(
        p.max_concurrent - running,
        &job_concurrency,
        &p.system.queue_concurrency,
        p.system.priority_decay,
    )
    .with_queues(p.system.queues.as_deref());

    let claimed = claim_pending_jobs(&mut conn, &params)?;
    drop(conn);

    for job_run in claimed {
        let Some(job_def) =
            resolve_job_def(&p.registry, &job_run, &p.pool, &p.system.queue_timeouts)
        else {
            continue;
        };

        spawn_job_execution(p, &job_run, job_def);
    }

    Ok(())
}

/// Read per-slug concurrency limits — sourced from
/// `crap.jobs.define({ concurrency = N })` on each user-defined
/// job. System jobs (`_system_image_convert`, `_system_email`) aren't
/// in the registry; their aggregate throttling is handled by the
/// per-queue cap mechanism (`[jobs.queues.images] concurrency = N`).
#[cfg(not(tarpaulin_include))]
fn read_job_concurrency(registry: &Registry) -> HashMap<String, u32> {
    registry
        .jobs
        .iter()
        .map(|(slug, def)| (slug.to_string(), def.concurrency))
        .collect()
}

/// Claim pending jobs, using IMMEDIATE transaction for `SQLite`.
#[cfg(not(tarpaulin_include))]
fn claim_pending_jobs(conn: &mut BoxedConnection, params: &ClaimParams<'_>) -> Result<Vec<JobRun>> {
    // One transaction path for BOTH backends: the
    // `FOR UPDATE SKIP LOCKED` row locks (Postgres) and the IMMEDIATE
    // write lock (SQLite) must be held across the whole select-count-claim
    // sequence, or the per-slug/per-queue concurrency caps are only
    // advisory across concurrent claimers. `transaction_immediate` is
    // plain BEGIN on Postgres (MVCC needs no IMMEDIATE) and IMMEDIATE on
    // SQLite.
    let tx = conn
        .transaction_immediate()
        .context("Failed to start claim transaction")?;
    let result = job_query::claim_pending_jobs_with(&tx, params)?;
    tx.commit().context("Failed to commit claim transaction")?;
    Ok(result)
}

/// A synthesized definition for a system job, which the registry does not
/// hold, with its queue's configured timeout or the framework default.
fn system_job_def(
    slug: &str,
    queue: &str,
    queue_timeouts: &HashMap<String, u64>,
    default_timeout: u64,
) -> Arc<JobDefinition> {
    let timeout = queue_timeouts
        .get(queue)
        .copied()
        .unwrap_or(default_timeout);

    Arc::new(
        JobDefinition::builder(slug, "_system")
            .queue(queue)
            .timeout(timeout)
            .build(),
    )
}

/// Resolve the job definition for a claimed job run.
#[cfg(not(tarpaulin_include))]
fn resolve_job_def(
    registry: &Registry,
    job_run: &JobRun,
    pool: &DbPool,
    queue_timeouts: &HashMap<String, u64>,
) -> Option<Arc<JobDefinition>> {
    if let Some(def) = registry.get_job(&job_run.slug) {
        return Some(def.clone());
    }

    // The system jobs are dispatched by `execute_job` directly (no Lua VM);
    // a synthesized definition lets them flow through the standard
    // claim/execute path. Per-queue concurrency throttling is handled via
    // `[jobs.queues.<queue>] concurrency = N`.
    let system = match job_run.slug.as_str() {
        SYSTEM_EMAIL_JOB => Some((SYSTEM_EMAIL_QUEUE, DEFAULT_EMAIL_QUEUE_TIMEOUT_SECS)),
        SYSTEM_IMAGE_CONVERT_JOB => Some((IMAGE_CONVERT_QUEUE, DEFAULT_IMAGES_QUEUE_TIMEOUT_SECS)),
        SYSTEM_BULK_JOB => Some((SYSTEM_BULK_QUEUE, DEFAULT_BULK_QUEUE_TIMEOUT_SECS)),
        _ => None,
    };

    if let Some((queue, default_timeout)) = system {
        return Some(system_job_def(
            &job_run.slug,
            queue,
            queue_timeouts,
            default_timeout,
        ));
    }

    warn!(
        "Job definition '{}' not found, marking as failed",
        job_run.slug
    );

    stamp_failed_run(pool, job_run, "job definition not found", false);

    None
}

/// Move a claimed run out of `running` with a guarded `fail_job` (compare-
/// and-set on running+attempt), on a write-pool connection like every other
/// job-row write. A run left `running` would permanently consume a
/// concurrency slot, so a failure to stamp it is logged loudly — the stale
/// recovery reclaims it once its heartbeat expires.
#[cfg(not(tarpaulin_include))]
fn stamp_failed_run(pool: &DbPool, job_run: &JobRun, reason: &str, should_retry: bool) {
    let conn = match pool.write() {
        Ok(conn) => conn,
        Err(e) => {
            warn!(
                "Failed to mark job {} as failed: no write connection: {e}",
                job_run.id
            );

            return;
        }
    };

    let _ = job_query::fail_job(&conn, &job_run.id, reason, should_retry, job_run.attempt)
        .inspect_err(|e| warn!("Failed to mark job {} as failed: {e}", job_run.id));

    // A bulk run failed here drops its request payload like every other
    // terminal bulk run.
    if job_run.slug == SYSTEM_BULK_JOB && !should_retry {
        strip_finished_payload(&conn, job_run);
    }
}

/// Whether a job's run stops itself at its `timeout` and may then still
/// need the self-limiting grace (a rollback, a terminal status write): a Lua
/// handler (the VM's cooperative deadline) and `_system_bulk` (its in-batch
/// deadline) do. Email delivery and image conversion get no grace: email is
/// bounded by its provider — the SMTP and webhook transport timeouts, or,
/// for a custom Lua provider, the queue timeout installed as the leased VM's
/// deadline, which ends the send with nothing left to roll back — and image
/// conversion by a finite encode.
fn enforces_own_deadline(slug: &str) -> bool {
    slug != SYSTEM_EMAIL_JOB && slug != SYSTEM_IMAGE_CONVERT_JOB
}

/// How long a run may execute before the watchdog reports it overdue.
///
/// A run that stops itself at its deadline gets the self-limiting grace on
/// top, covering the rollback and the terminal status write after the
/// deadline, so only a genuinely stuck run is reported; the rest are
/// reported once their timeout has passed.
fn watchdog_after(job_def: &JobDefinition) -> Duration {
    let secs = if enforces_own_deadline(&job_def.slug) {
        job_def.timeout.saturating_add(SELF_LIMITING_JOB_GRACE_SECS)
    } else {
        job_def.timeout
    };

    Duration::from_secs(secs)
}

/// Await a blocking run to its end, calling `on_overdue` once if it is
/// still executing after `watchdog`.
///
/// The wait is never abandoned: dropping the handle would not stop the
/// blocking thread, only hide it, and the caller would then release the
/// run's row while its handler still commits.
async fn await_run<T>(
    mut handle: JoinHandle<T>,
    watchdog: Duration,
    on_overdue: impl FnOnce(),
) -> Result<T, JoinError> {
    if let Ok(joined) = timeout(watchdog, &mut handle).await {
        return joined;
    }

    on_overdue();

    handle.await
}

/// A claimed run with everything its blocking execution owns.
struct ClaimedRun {
    pool: DbPool,
    hook_runner: HookRunner,
    job_def: Arc<JobDefinition>,
    job_run: JobRun,
    email_provider: Option<SharedEmailProvider>,
    storage: SharedStorage,
    lua_infra: LuaCrudInfra,
    app_infra: Arc<AppInfra>,
}

impl ClaimedRun {
    /// Everything `job_run` needs from the poll's shared input.
    fn from_poll(p: &PollInput, job_run: &JobRun, job_def: Arc<JobDefinition>) -> Self {
        Self {
            pool: p.pool.clone(),
            hook_runner: p.hook_runner.clone(),
            job_def,
            job_run: job_run.clone(),
            email_provider: p.email_provider.clone(),
            storage: p.system.storage.clone(),
            lua_infra: p.system.lua_infra.clone(),
            app_infra: Arc::clone(&p.system.app_infra),
        }
    }
}

/// The blocking body of one run.
fn execute_claimed(run: &ClaimedRun) -> Result<()> {
    execute_job(ExecuteJobParams {
        pool: &run.pool,
        hook_runner: &run.hook_runner,
        job_def: &run.job_def,
        job_run: &run.job_run,
        email_provider: run.email_provider.as_deref(),
        storage: &run.storage,
        lua_infra: Some(&run.lua_infra),
        app_infra: Some(&run.app_infra),
    })
}

/// The reason to stamp a run `failed` with, for an outcome that did not
/// record a terminal status itself. `execute_job` writes one on its normal
/// success / handler-error paths and returns `Ok(())`; it returns `Err`
/// ONLY from an early path that ran before writing one (missing provider,
/// bad job data, a post-handler `pool.write` failure), and a panic leaves
/// the row `running` too — without the stamp those runs would stick in
/// `running` forever, permanently consuming a concurrency slot.
fn failure_reason(job_run: &JobRun, outcome: &Result<Result<()>, JoinError>) -> Option<String> {
    match outcome {
        Ok(Ok(())) => None,
        Ok(Err(e)) => {
            error!(
                "Job {} ({}) execution error: {}",
                job_run.id, job_run.slug, e
            );

            Some(format!("execution error: {e}"))
        }
        Err(e) => {
            error!("Job {} ({}) panicked: {}", job_run.id, job_run.slug, e);

            Some(format!("handler panicked: {e}"))
        }
    }
}

/// Log a run that is still executing past its watchdog.
fn report_overdue(job_run: &JobRun, watchdog: Duration) {
    error!(
        "Job {} ({}) is still executing {}s after it started, past its timeout — it stays \
         `running` and keeps its concurrency slot until its handler returns, and is not \
         retried while it runs",
        job_run.id,
        job_run.slug,
        watchdog.as_secs()
    );
}

/// Stop tracking a finished run and wake the loop, so the slot it held is
/// claimed again now rather than at the next poll tick.
fn release_run(running_jobs: &RunningJobs, run_finished: &Notify, id: &str) {
    if let Ok(mut guard) = running_jobs.lock() {
        guard.retain(|job| job.id != id);
    }

    run_finished.notify_one();
}

/// Releases a tracked run when dropped — also when its task panics. A run
/// left tracked after its task is gone would have its heartbeat refreshed
/// forever, so stale recovery could never reclaim its row and the slot it
/// holds would never free.
struct RunRelease {
    running_jobs: RunningJobs,
    run_finished: Arc<Notify>,
    id: String,
}

impl Drop for RunRelease {
    fn drop(&mut self) {
        release_run(&self.running_jobs, &self.run_finished, &self.id);
    }
}

/// Execute one claimed run to its end, watched by its watchdog, and stamp it
/// `failed` when it ended without recording an outcome of its own.
async fn watch_run(run: ClaimedRun) {
    let watchdog = watchdog_after(&run.job_def);
    let job_run = run.job_run.clone();
    let pool = run.pool.clone();

    let handle = spawn_blocking(move || execute_claimed(&run));
    let outcome = await_run(handle, watchdog, || report_overdue(&job_run, watchdog)).await;

    if let Some(reason) = failure_reason(&job_run, &outcome) {
        let should_retry = job_run.attempt < job_run.max_attempts;

        stamp_failed_run(&pool, &job_run, &reason, should_retry);
    }
}

/// Start one claimed run on the blocking pool, tracked (a shutdown waits
/// for it rather than dropping it mid-transaction) and watched to its end.
/// The run stays in `running_jobs` — its heartbeat kept fresh — until it has
/// actually ended.
#[cfg(not(tarpaulin_include))]
fn spawn_job_execution(p: &PollInput, job_run: &JobRun, job_def: Arc<JobDefinition>) {
    if let Ok(mut guard) = p.running_jobs.lock() {
        guard.push(RunningJob::new(job_run.id.clone(), job_run.attempt));
    }

    let run = ClaimedRun::from_poll(p, job_run, job_def);
    let release = RunRelease {
        running_jobs: p.running_jobs.clone(),
        run_finished: Arc::clone(&p.run_finished),
        id: job_run.id.clone(),
    };

    p.system.job_tasks.spawn(async move {
        let _release = release;

        watch_run(run).await;
    });
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Mutex,
            atomic::AtomicUsize,
            mpsc::{self, Receiver},
        },
        thread,
    };

    use tokio::task::{spawn, yield_now};

    use super::*;

    /// A request on an idle gate starts a poll; one made while it runs is
    /// served by another pass of the same poll; once the passes are done the
    /// gate is free again.
    #[test]
    fn a_request_during_a_poll_is_served_by_one_more_pass() {
        let gate = PollGate::new();

        assert!(gate.request(), "an idle gate starts a poll");
        gate.begin_pass();

        assert!(!gate.request(), "a second poll never starts while one runs");
        assert!(gate.finish_pass(), "the in-flight poll runs another pass");

        gate.begin_pass();
        assert!(!gate.finish_pass(), "no request left: the gate is released");

        assert!(gate.request(), "a released gate starts the next poll");
    }

    /// Requests that pile up during one pass are served by ONE more pass, not
    /// one pass each.
    #[test]
    fn requests_during_a_pass_collapse_into_one_more_pass() {
        let gate = PollGate::new();

        assert!(gate.request());
        gate.begin_pass();

        for _ in 0..5 {
            assert!(!gate.request());
        }

        assert!(gate.finish_pass());
        gate.begin_pass();
        assert!(!gate.finish_pass());
    }

    /// A pass that blocks until released, counting how often it ran.
    fn blocking_pass(
        release: Receiver<()>,
    ) -> (Arc<AtomicUsize>, impl Fn() + Send + Sync + 'static) {
        let runs = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&runs);
        let release = Mutex::new(release);

        let pass = move || {
            counter.fetch_add(1, Ordering::SeqCst);

            // Only the first pass waits; later ones run straight through.
            if counter.load(Ordering::SeqCst) == 1 {
                release
                    .lock()
                    .expect("release lock")
                    .recv()
                    .expect("released");
            }
        };

        (runs, pass)
    }

    /// Regression: a run finishing while a poll was in flight woke the loop,
    /// but the single-flight guard simply skipped that poll — the freed slot
    /// then sat idle until the next poll tick, so a queue of fast runs with
    /// `concurrency = 1` drained at one run per tick. The wake-up now gets a
    /// pass of its own.
    #[tokio::test]
    async fn a_wake_up_during_an_in_flight_poll_is_not_lost() {
        let tasks = TaskTracker::new();
        let gate = Arc::new(PollGate::new());
        let (release, released) = mpsc::channel::<()>();
        let (runs, pass) = blocking_pass(released);
        let pass = Arc::new(pass);

        let first = Arc::clone(&pass);
        spawn_gated(&tasks, &gate, move || first());

        // Wait until the first pass is running, then request again.
        while runs.load(Ordering::SeqCst) == 0 {
            thread::yield_now();
            yield_now().await;
        }

        let second = Arc::clone(&pass);
        spawn_gated(&tasks, &gate, move || second());

        release.send(()).expect("release the first pass");
        tasks.close();
        tasks.wait().await;

        assert_eq!(
            runs.load(Ordering::SeqCst),
            2,
            "the request made during the first pass must get its own pass"
        );
        assert!(
            gate.request(),
            "the gate is released once the passes are done"
        );
    }

    /// A panicking pass releases the gate, so polling resumes.
    #[tokio::test]
    async fn a_panicking_pass_releases_the_gate() {
        let tasks = TaskTracker::new();
        let gate = Arc::new(PollGate::new());

        spawn_gated(&tasks, &gate, || panic!("poll failed"));

        tasks.close();
        tasks.wait().await;

        assert!(gate.request(), "the gate must be free after a panic");
    }

    /// Regression: the scheduler's timer abandoned a run that outlived its
    /// timeout — the blocking thread kept executing while the row was failed
    /// and re-queued, so the retry ran next to it. The watchdog now reports
    /// the overrun and keeps waiting: the caller learns the outcome only once
    /// the run has actually ended.
    #[tokio::test]
    async fn an_overdue_run_is_reported_but_awaited_to_its_end() {
        let finished = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&finished);
        let overdue = Arc::new(AtomicBool::new(false));
        let reported = Arc::clone(&overdue);

        let handle = spawn_blocking(move || {
            thread::sleep(Duration::from_millis(150));
            flag.store(true, Ordering::SeqCst);

            7
        });

        let result = await_run(handle, Duration::from_millis(10), || {
            reported.store(true, Ordering::SeqCst);
        })
        .await;

        assert!(
            overdue.load(Ordering::SeqCst),
            "the overrun must be reported"
        );
        assert!(
            finished.load(Ordering::SeqCst),
            "the wait returned before the run had ended"
        );
        assert_eq!(result.expect("the run completes"), 7);
    }

    /// A run that ends in time is not reported.
    #[tokio::test]
    async fn a_run_within_its_watchdog_is_not_reported() {
        let handle = spawn_blocking(|| 1);

        let result = await_run(handle, Duration::from_mins(1), || {
            panic!("a timely run must not be reported overdue")
        })
        .await;

        assert_eq!(result.expect("the run completes"), 1);
    }

    /// Runs that stop themselves at their deadline are watched with the
    /// self-limiting grace on top; email and image runs at their timeout.
    #[test]
    fn the_watchdog_adds_grace_only_for_self_limiting_runs() {
        let lua_job = JobDefinition::builder("reports", "jobs.reports.run")
            .timeout(30)
            .build();
        let bulk = JobDefinition::builder(SYSTEM_BULK_JOB, "_system")
            .timeout(30)
            .build();
        let email = JobDefinition::builder(SYSTEM_EMAIL_JOB, "_system")
            .timeout(30)
            .build();
        let image = JobDefinition::builder(SYSTEM_IMAGE_CONVERT_JOB, "_system")
            .timeout(30)
            .build();

        let graced = Duration::from_secs(30 + SELF_LIMITING_JOB_GRACE_SECS);

        assert_eq!(watchdog_after(&lua_job), graced);
        assert_eq!(watchdog_after(&bulk), graced);
        assert_eq!(watchdog_after(&email), Duration::from_secs(30));
        assert_eq!(watchdog_after(&image), Duration::from_secs(30));
    }

    /// A finished run leaves the tracked set (so its heartbeat stops) and
    /// wakes the loop.
    #[tokio::test]
    async fn releasing_a_run_untracks_it_and_wakes_the_loop() {
        let running_jobs: RunningJobs = Arc::new(Mutex::new(vec![
            RunningJob::new("a".to_string(), 1),
            RunningJob::new("b".to_string(), 2),
        ]));
        let run_finished = Notify::new();

        release_run(&running_jobs, &run_finished, "a");

        assert_eq!(
            *running_jobs.lock().expect("running jobs"),
            vec![RunningJob::new("b".to_string(), 2)]
        );

        // The permit is stored even though nothing was waiting yet.
        timeout(Duration::from_secs(5), run_finished.notified())
            .await
            .expect("the loop must be woken");
    }

    /// Regression guard: a run's task that panics after the run was tracked
    /// still untracks it. Left tracked, its heartbeat would be refreshed
    /// forever and stale recovery could never reclaim the row or its slot.
    #[tokio::test]
    async fn a_panicking_run_task_still_releases_the_run() {
        let running_jobs: RunningJobs =
            Arc::new(Mutex::new(vec![RunningJob::new("a".to_string(), 1)]));
        let run_finished = Arc::new(Notify::new());
        let release = RunRelease {
            running_jobs: running_jobs.clone(),
            run_finished: Arc::clone(&run_finished),
            id: "a".to_string(),
        };

        let task = spawn(async move {
            let _release = release;

            panic!("the watch failed");
        });

        assert!(task.await.is_err(), "the task must have panicked");
        assert!(running_jobs.lock().expect("running jobs").is_empty());

        timeout(Duration::from_secs(5), run_finished.notified())
            .await
            .expect("the loop must be woken");
    }
}
