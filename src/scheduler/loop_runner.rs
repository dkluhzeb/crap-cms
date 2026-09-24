//! Scheduler event loop — polls jobs, evaluates cron, manages heartbeats.
//!
//! The `select!` loop itself only schedules: every tick's database work runs
//! on the blocking pool through [`spawn_tick`], so a tick that waits on the
//! database — a heartbeat behind a long write, a purge over many rows —
//! delays neither the shutdown arm nor the other ticks. Each tick is
//! single-flighted: a tick whose predecessor is still running is skipped.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use anyhow::Result;
use chrono::{DateTime, Utc};
use tokio::{
    select,
    sync::Notify,
    task::spawn_blocking,
    time::{Duration, Interval, interval, timeout},
};
use tokio_util::task::TaskTracker;
use tracing::{error, info, warn};

use crate::{
    config::JobsConfig,
    core::{Registry, SharedStorage},
    db::DbPool,
    hooks::LuaCrudInfra,
    service::AppInfra,
};

use super::{
    announce::{StartupAnnounce, announce_and_recover},
    cron_tick::{CronMode, CronTickInput, PurgeSchedule, cron_mode, cron_tick},
    heartbeat::{HeartbeatTickInput, heartbeat_tick, stale_threshold_secs},
    poll::{PollGate, PollInput, spawn_poll},
    types::{RunningJobs, SchedulerParams, TickJobConfig},
};

/// The poll, cron and heartbeat tickers of the loop, from their configured
/// periods in seconds.
fn tickers(poll_secs: u64, cron_secs: u64, heartbeat_secs: u64) -> (Interval, Interval, Interval) {
    (
        interval(Duration::from_secs(poll_secs)),
        interval(Duration::from_secs(cron_secs)),
        interval(Duration::from_secs(heartbeat_secs)),
    )
}

/// Announce the scheduler, reclaim what a previous process left `running`,
/// and only then let a readiness probe answer OK — the rest of the process
/// describes reality only after that recovery. Returns the stale threshold
/// the heartbeat tick reuses (a `running` job is assumed dead once its
/// heartbeat is older than it).
fn announce_recover_and_ready(infra: &AppInfra, announce: &StartupAnnounce<'_>) -> Result<u64> {
    announce_and_recover(announce)?;
    infra.readiness.mark_ready();

    Ok(announce.stale_threshold_secs)
}

/// The loop's tickers, single-flight guards and task tracker.
///
/// Every tick task and every job task the poll spawns is tracked in
/// `job_tasks`, so the shutdown arm can wait for the work already in flight;
/// without it a stop drops a running job mid-transaction (the row keeps a
/// fresh heartbeat, a no-retry run goes terminally stale, and its post-commit
/// effects never happen). The single-flight guards matter most for the poll
/// (see [`PollGate`]): two overlapping polls each read the same stale
/// `count_running` before either claim commits and each claim up to
/// `available`, pushing running past the global `max_concurrent` cap.
/// `run_finished` is woken by every run that ends, so the freed slot is
/// claimed again at once instead of at the next poll tick.
struct LoopClocks {
    poll_ticker: Interval,
    cron_ticker: Interval,
    heartbeat_ticker: Interval,
    running_jobs: RunningJobs,
    job_tasks: TaskTracker,
    drain_deadline: Duration,
    poll_gate: Arc<PollGate>,
    run_finished: Arc<Notify>,
    cron_in_flight: Arc<AtomicBool>,
    heartbeat_in_flight: Arc<AtomicBool>,
    last_cron_check: Arc<Mutex<DateTime<Utc>>>,
    purge_counter: Arc<AtomicU64>,
}

impl LoopClocks {
    fn new(config: &JobsConfig) -> Self {
        let (poll_ticker, cron_ticker, heartbeat_ticker) = tickers(
            config.poll_interval,
            config.cron_interval,
            config.heartbeat_interval,
        );

        Self {
            poll_ticker,
            cron_ticker,
            heartbeat_ticker,
            running_jobs: Arc::new(Mutex::new(Vec::new())),
            job_tasks: TaskTracker::new(),
            drain_deadline: Duration::from_secs(config.drain_deadline_secs()),
            poll_gate: Arc::new(PollGate::new()),
            run_finished: Arc::new(Notify::new()),
            cron_in_flight: Arc::new(AtomicBool::new(false)),
            heartbeat_in_flight: Arc::new(AtomicBool::new(false)),
            last_cron_check: Arc::new(Mutex::new(Utc::now())),
            purge_counter: Arc::new(AtomicU64::new(0)),
        }
    }
}

/// What every cron tick shares: the retry defaults, the last-check clock,
/// the purge cadence counter and the cron mode.
struct CronShared {
    queue_retries: Arc<HashMap<String, u32>>,
    last_cron_check: Arc<Mutex<DateTime<Utc>>>,
    purge_counter: Arc<AtomicU64>,
    mode: CronMode,
}

impl CronShared {
    fn input(&self, infra: &Arc<AppInfra>, config: &JobsConfig) -> CronTickInput {
        CronTickInput {
            infra: Arc::clone(infra),
            queue_retries: Arc::clone(&self.queue_retries),
            last_cron_check: Arc::clone(&self.last_cron_check),
            purge: PurgeSchedule {
                counter: Arc::clone(&self.purge_counter),
                auto_purge_secs: config.auto_purge,
                cron_interval_secs: i64::try_from(config.cron_interval).unwrap_or(i64::MAX),
            },
            mode: self.mode,
        }
    }
}

/// One heartbeat tick's input — fresh handles, the shared running-job list
/// and the stale threshold the recovery half compares against.
fn heartbeat_input(
    pool: &DbPool,
    registry: &Arc<Registry>,
    running_jobs: &RunningJobs,
    stale_threshold_secs: u64,
) -> HeartbeatTickInput {
    HeartbeatTickInput {
        pool: pool.clone(),
        registry: Arc::clone(registry),
        running_jobs: running_jobs.clone(),
        stale_threshold_secs,
    }
}

/// Start the scheduler background loop. Runs until the cancellation token fires.
///
/// # Errors
///
/// Returns an error if the connection acquisition or stale-job recovery
/// step at startup fails.
#[cfg(not(tarpaulin_include))]
pub async fn start(params: SchedulerParams) -> Result<()> {
    let SchedulerParams {
        infra,
        config,
        db_timeouts,
        shutdown,
        email_provider,
        queues,
        run_cron,
    } = params;

    // Unpack the core deps the loop threads through its helpers (cheap Arc
    // clones); the scheduler uses only this subset of the bundle.
    let pool = infra.pool.clone();
    let hook_runner = infra.hook_runner.clone();
    let registry = Arc::clone(&infra.registry);
    let storage = infra.storage.clone();
    let job_lua_infra = job_crud_infra(&infra);

    let stale_threshold_secs = announce_recover_and_ready(
        &infra,
        &StartupAnnounce {
            config: &config,
            pool: &pool,
            registry: &registry,
            stale_threshold_secs: stale_threshold_secs(config.heartbeat_interval, &db_timeouts),
            queues: queues.as_deref(),
            run_cron,
        },
    )?;

    // Shared with every poll tick, which hands it to the claim query.
    let queues = queues.map(Arc::<[String]>::from);

    let QueueMaps {
        queue_concurrency,
        queue_timeouts,
        queue_retries,
    } = build_queue_maps(&config);

    let LoopClocks {
        mut poll_ticker,
        mut cron_ticker,
        mut heartbeat_ticker,
        running_jobs,
        job_tasks,
        drain_deadline,
        poll_gate,
        run_finished,
        cron_in_flight,
        heartbeat_in_flight,
        last_cron_check,
        purge_counter,
    } = LoopClocks::new(&config);
    let cron = CronShared {
        queue_retries: Arc::new(queue_retries),
        last_cron_check,
        purge_counter,
        mode: cron_mode(run_cron),
    };

    let poll_input = Arc::new(PollInput {
        pool: pool.clone(),
        hook_runner,
        registry: Arc::clone(&registry),
        max_concurrent: config.max_concurrent,
        running_jobs: running_jobs.clone(),
        email_provider,
        run_finished: Arc::clone(&run_finished),
        system: tick_job_config(&TickConfigSource {
            infra: &infra,
            priority_decay: config.priority_decay,
            queue_concurrency: &queue_concurrency,
            queue_timeouts: &queue_timeouts,
            queues: queues.as_ref(),
            storage: &storage,
            lua_infra: &job_lua_infra,
            job_tasks: &job_tasks,
        }),
    });

    loop {
        select! {
            () = shutdown.cancelled() => {
                info!("Scheduler shutting down");

                drain_job_tasks(&job_tasks, &running_jobs, drain_deadline).await;

                break Ok(());
            }
            _ = poll_ticker.tick() => {
                spawn_poll(&job_tasks, &poll_gate, &poll_input);
            }
            // A run ended: its slot is free now, not at the next poll tick.
            () = run_finished.notified() => {
                spawn_poll(&job_tasks, &poll_gate, &poll_input);
            }
            _ = cron_ticker.tick() => {
                let input = cron.input(&infra, &config);

                spawn_tick(&job_tasks, &cron_in_flight, "cron", move || cron_tick(&input));
            }
            _ = heartbeat_ticker.tick() => {
                let input = heartbeat_input(&pool, &registry, &running_jobs, stale_threshold_secs);

                spawn_tick(&job_tasks, &heartbeat_in_flight, "heartbeat", move || {
                    heartbeat_tick(&input);
                });
            }
            // Image conversion now runs through the unified job queue as
            // `_system_image_convert` system jobs — see
            // `runner::execute_system_image_convert`. The poll arms above
            // pick them up alongside email and Lua-handler jobs. No
            // dedicated image ticker / single-flight gate needed; the job
            // queue's `max_concurrent` + per-slug `job_concurrency` map
            // handle worker throttling.
        }
    }
}

/// Run one tick's blocking body on the blocking pool, tracked like a job task
/// and single-flighted: a tick whose predecessor is still in flight is
/// skipped rather than stacked behind it. The flag is released once the body
/// has finished — or panicked — so a stuck tick can never wedge the flag.
///
/// The body never runs on the loop's own runtime thread: a tick that waits
/// on the database (a heartbeat behind a long write, a purge over many rows)
/// would otherwise hold up the shutdown arm and every other tick with it —
/// including the heartbeats whose delay the stale threshold exists to bound.
fn spawn_tick<F>(tasks: &TaskTracker, in_flight: &Arc<AtomicBool>, name: &'static str, body: F)
where
    F: FnOnce() + Send + 'static,
{
    if in_flight.swap(true, Ordering::SeqCst) {
        return;
    }

    let in_flight = Arc::clone(in_flight);

    tasks.spawn(async move {
        if let Err(e) = spawn_blocking(body).await {
            error!("Scheduler {name} tick panicked: {e}");
        }

        in_flight.store(false, Ordering::SeqCst);
    });
}

/// Event transport, populate cache and email context for user job handlers'
/// Lua CRUD calls, so job writes publish live-update events, invalidate the
/// populate cache and issue account verifications like every other surface.
/// The event queue is injected per invocation by `run_job_handler` (which
/// flushes it post-handler).
fn job_crud_infra(infra: &AppInfra) -> LuaCrudInfra {
    LuaCrudInfra::for_pool_crud(infra)
}

/// Pre-built per-queue lookup maps derived from `JobsConfig.queues`.
/// Built once at scheduler startup so each poll / cron tick reuses the
/// same snapshot — config is immutable at runtime, so re-allocating per
/// tick would burn cycles. Fields keep the `queue_` prefix to match
/// the names used at the call sites and avoid mismatched lookups.
#[allow(clippy::struct_field_names)]
struct QueueMaps {
    /// Per-queue aggregate concurrency caps; entries with `0`
    /// (unlimited) are filtered out — `claim_pending_jobs` treats a
    /// missing key as "no per-queue cap."
    queue_concurrency: HashMap<String, u32>,
    /// Per-queue timeouts for system jobs (`_system_email`,
    /// `_system_image_convert`). Entries with `0` are filtered out —
    /// `resolve_job_def` falls back to a hardcoded default per slug.
    queue_timeouts: HashMap<String, u64>,
    /// Per-queue retry budgets. Only queues with an explicit
    /// `Some(N)` retries entry appear here;
    /// `JobDefinition::effective_max_attempts` falls back to `0` (one
    /// attempt) when neither the definition nor this map supplies a
    /// value.
    queue_retries: HashMap<String, u32>,
}

/// Snapshot `[jobs.queues]` into the three lookup maps used by the
/// scheduler's poll / cron / Lua-queue paths.
fn build_queue_maps(config: &JobsConfig) -> QueueMaps {
    let queue_concurrency = config
        .queues
        .iter()
        .filter_map(|(name, q)| {
            let c = q.effective_concurrency();
            (c > 0).then(|| (name.clone(), c))
        })
        .collect();

    let queue_timeouts = config
        .queues
        .iter()
        .filter_map(|(name, q)| {
            let t = q.effective_timeout();
            (t > 0).then(|| (name.clone(), t))
        })
        .collect();

    let queue_retries = config.queue_retries();

    QueueMaps {
        queue_concurrency,
        queue_timeouts,
        queue_retries,
    }
}

/// Borrowed sources for one [`TickJobConfig`] snapshot.
struct TickConfigSource<'a> {
    infra: &'a Arc<AppInfra>,
    priority_decay: u64,
    queue_concurrency: &'a HashMap<String, u32>,
    queue_timeouts: &'a HashMap<String, u64>,
    queues: Option<&'a Arc<[String]>>,
    storage: &'a SharedStorage,
    lua_infra: &'a LuaCrudInfra,
    job_tasks: &'a TaskTracker,
}

/// Snapshot the execution config (cheap clones) every poll shares.
fn tick_job_config(s: &TickConfigSource<'_>) -> TickJobConfig {
    TickJobConfig {
        app_infra: Arc::clone(s.infra),
        priority_decay: s.priority_decay,
        queue_concurrency: s.queue_concurrency.clone(),
        queue_timeouts: s.queue_timeouts.clone(),
        queues: s.queues.cloned(),
        storage: s.storage.clone(),
        lua_infra: s.lua_infra.clone(),
        job_tasks: s.job_tasks.clone(),
    }
}

/// Wait for the job tasks already in flight when the shutdown token fired.
///
/// The tracker is closed first so `wait()` can complete; the poll ticker is
/// no longer running, so nothing new is claimed. `deadline` comes from the
/// configured job timeouts — a run that outlives it has already blown past
/// its own timeout; it dies with the process, its row still `running`, and
/// stale recovery reclaims it once its heartbeat has stopped.
async fn drain_job_tasks(job_tasks: &TaskTracker, running_jobs: &RunningJobs, deadline: Duration) {
    job_tasks.close();

    if job_tasks.is_empty() {
        return;
    }

    let ids: Vec<String> = running_jobs
        .lock()
        .map(|guard| guard.iter().map(|job| job.id.clone()).collect())
        .unwrap_or_default();

    info!(
        "Waiting up to {}s for {} in-flight scheduler task(s) to finish (jobs: {})",
        deadline.as_secs(),
        job_tasks.len(),
        if ids.is_empty() {
            "none claimed yet".to_string()
        } else {
            ids.join(", ")
        }
    );

    if timeout(deadline, job_tasks.wait()).await.is_err() {
        warn!(
            "{} scheduler task(s) still running after {}s — exiting anyway; \
             stale-job recovery will reclaim whatever they left behind",
            job_tasks.len(),
            deadline.as_secs()
        );

        return;
    }

    info!("All in-flight scheduler tasks finished");
}

#[cfg(test)]
mod tests {
    use std::{future::pending, sync::mpsc, time::Instant};

    use tokio::time::sleep;

    use crate::{
        config::{JobsConfig, QueueConfig},
        scheduler::types::RunningJob,
    };

    use super::*;

    /// A tick whose predecessor is still running is skipped, and the flag is
    /// released once that predecessor finishes — so ticks never stack behind
    /// a slow one, and a slow one never wedges the flag.
    #[tokio::test]
    async fn a_tick_in_flight_skips_the_next_and_then_releases() {
        let tasks = TaskTracker::new();
        let in_flight = Arc::new(AtomicBool::new(false));
        let (release, released) = mpsc::channel::<()>();
        let second_ran = Arc::new(AtomicBool::new(false));

        spawn_tick(&tasks, &in_flight, "test", move || {
            released.recv().expect("released");
        });

        let flag = Arc::clone(&second_ran);
        spawn_tick(&tasks, &in_flight, "test", move || {
            flag.store(true, Ordering::SeqCst);
        });

        release.send(()).expect("release the first tick");
        tasks.close();
        tasks.wait().await;

        assert!(
            !second_ran.load(Ordering::SeqCst),
            "the second tick must be skipped while the first is in flight"
        );
        assert!(
            !in_flight.load(Ordering::SeqCst),
            "the flag is released once the tick finishes"
        );
    }

    /// A panicking tick body releases the flag too.
    #[tokio::test]
    async fn a_panicking_tick_releases_the_flag() {
        let tasks = TaskTracker::new();
        let in_flight = Arc::new(AtomicBool::new(false));

        spawn_tick(&tasks, &in_flight, "test", || panic!("tick failed"));

        tasks.close();
        tasks.wait().await;

        assert!(!in_flight.load(Ordering::SeqCst));
    }

    /// The shutdown arm must not return while a claimed run is still
    /// executing: dropping it would leave a `running` row with a fresh
    /// heartbeat and skip its post-commit effects.
    #[tokio::test]
    async fn the_drain_waits_for_a_task_that_finishes_after_the_token() {
        let job_tasks = TaskTracker::new();
        let finished = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&finished);

        job_tasks.spawn(async move {
            sleep(Duration::from_millis(40)).await;

            flag.store(true, Ordering::SeqCst);
        });

        let running_jobs: RunningJobs =
            Arc::new(Mutex::new(vec![RunningJob::new("job-1".to_string(), 1)]));

        drain_job_tasks(&job_tasks, &running_jobs, Duration::from_secs(30)).await;

        assert!(
            finished.load(Ordering::SeqCst),
            "the drain returned before the in-flight job task finished"
        );
    }

    /// A task that never finishes must not hold the process open forever —
    /// the deadline bounds the wait.
    #[tokio::test]
    async fn the_drain_gives_up_once_the_deadline_passes() {
        let job_tasks = TaskTracker::new();
        job_tasks.spawn(pending::<()>());

        let running_jobs = Arc::new(Mutex::new(Vec::new()));
        let started = Instant::now();

        drain_job_tasks(&job_tasks, &running_jobs, Duration::from_millis(30)).await;

        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the drain ignored its deadline"
        );
    }

    /// Nothing in flight: the drain is a no-op, not a wait.
    #[tokio::test]
    async fn the_drain_returns_immediately_when_nothing_is_running() {
        let job_tasks = TaskTracker::new();
        let running_jobs = Arc::new(Mutex::new(Vec::new()));
        let started = Instant::now();

        drain_job_tasks(&job_tasks, &running_jobs, Duration::from_hours(1)).await;

        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn build_queue_maps_applies_a_distinct_gate_per_map() {
        let mut config = JobsConfig::default();
        config.queues.insert(
            "fast".into(),
            QueueConfig {
                concurrency: Some(5),
                timeout: None,
                retries: Some(0),
            },
        );
        config.queues.insert(
            "slow".into(),
            QueueConfig {
                concurrency: Some(0),
                timeout: Some(30),
                retries: None,
            },
        );

        let maps = build_queue_maps(&config);

        // concurrency map: only effective > 0 (so Some(0) and None are excluded).
        assert_eq!(maps.queue_concurrency.get("fast"), Some(&5));
        assert!(!maps.queue_concurrency.contains_key("slow"));

        // timeout map: only effective > 0.
        assert_eq!(maps.queue_timeouts.get("slow"), Some(&30));
        assert!(!maps.queue_timeouts.contains_key("fast"));

        // retries map: any `Some(_)` is included — including Some(0); None excluded.
        assert_eq!(maps.queue_retries.get("fast"), Some(&0));
        assert!(!maps.queue_retries.contains_key("slow"));
    }
}
