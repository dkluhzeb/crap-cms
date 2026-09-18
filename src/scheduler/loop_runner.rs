//! Scheduler event loop — polls jobs, evaluates cron, processes images, manages heartbeats.

use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use anyhow::{Context as _, Result};
use chrono::Utc;
use tokio::{
    select,
    time::{Duration, Interval, interval, timeout},
};
use tokio_util::task::TaskTracker;
use tracing::{debug, error, info, warn};

use crate::{
    config::{
        DEFAULT_BULK_QUEUE_TIMEOUT_SECS, DEFAULT_EMAIL_QUEUE_TIMEOUT_SECS,
        DEFAULT_IMAGES_QUEUE_TIMEOUT_SECS, JobsConfig, LocaleConfig, SELF_LIMITING_JOB_GRACE_SECS,
    },
    core::{
        JobDefinition, JobRun, Registry, SharedEmailProvider, SharedStorage,
        email::{SYSTEM_EMAIL_JOB, SYSTEM_EMAIL_QUEUE},
        job::{SYSTEM_BULK_JOB, SYSTEM_BULK_QUEUE},
        upload::{IMAGE_CONVERT_QUEUE, SYSTEM_IMAGE_CONVERT_JOB, delete_storage_keys},
    },
    db::{BoxedConnection, DbPool, query::jobs as job_query},
    hooks::{HookRunner, LuaCrudInfra},
    service::AppInfra,
};

use super::{
    bulk::strip_finished_payload,
    runner::{
        ExecuteJobParams, check_cron_schedules, claim_retention_purge_tick, execute_job,
        purge_soft_deleted, recover_stale_jobs,
    },
    types::{SchedulerParams, TickJobConfig},
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
        shutdown,
        email_provider,
    } = params;

    // Unpack the core deps the loop threads through its helpers (cheap Arc
    // clones); the scheduler uses only this subset of the bundle.
    let pool = infra.pool.clone();
    let hook_runner = infra.hook_runner.clone();
    let registry = Arc::clone(&infra.registry);
    let storage = infra.storage.clone();
    let locale_config = infra.locale_config.clone();
    let job_lua_infra = job_crud_infra(&infra);

    // A `running` job is assumed dead once its heartbeat is older than this
    // (3× the interval = two missed beats of slack); the heartbeat tick below
    // reuses it to reclaim jobs from crashed nodes.
    let stale_threshold_secs = config
        .heartbeat_interval
        .saturating_mul(STALE_HEARTBEAT_MULTIPLIER);

    announce_and_recover(&config, &pool, &registry, stale_threshold_secs)?;

    // Startup recovery has rewritten every job row a previous process left
    // `running`; only now does the rest of the process describe reality, so
    // only now may a readiness probe answer OK.
    infra.readiness.mark_ready();

    let auto_purge_secs = config.auto_purge;
    let priority_decay = config.priority_decay;

    let QueueMaps {
        queue_concurrency,
        queue_timeouts,
        queue_retries,
    } = build_queue_maps(&config);

    let (mut poll_ticker, mut cron_ticker, mut heartbeat_ticker) = tickers(
        config.poll_interval,
        config.cron_interval,
        config.heartbeat_interval,
    );

    let running_jobs: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    // Every poll task and every job task it spawns is tracked here, so the
    // shutdown arm can wait for the work already in flight. Without it a stop
    // drops a running job mid-transaction: the row keeps a fresh heartbeat
    // (stale recovery waits the full window before reclaiming it), a no-retry
    // run goes terminally stale, and its post-commit effects never happen.
    let job_tasks = TaskTracker::new();
    let drain_deadline = Duration::from_secs(config.drain_deadline_secs());
    // Single-flight guard for the poll: at most one `poll_and_execute` runs at
    // a time. Without it, two overlapping poll tasks each read the same stale
    // `count_running` before either claim commits and each claim up to
    // `available`, pushing running past the global `max_concurrent` cap.
    let poll_in_flight = Arc::new(AtomicBool::new(false));
    let mut last_cron_check = Utc::now();
    let mut purge_counter: u64 = 0;

    loop {
        select! {
            () = shutdown.cancelled() => {
                info!("Scheduler shutting down");

                drain_job_tasks(&job_tasks, &running_jobs, drain_deadline).await;

                break Ok(());
            }
            _ = poll_ticker.tick() => {
                // Skip this tick if the previous poll is still in flight.
                if poll_in_flight.swap(true, Ordering::SeqCst) {
                    continue;
                }

                let pool = pool.clone();
                let hook_runner = hook_runner.clone();
                let registry = Arc::clone(&registry);
                let running_jobs = running_jobs.clone();
                let poll_in_flight = poll_in_flight.clone();
                let max_concurrent = config.max_concurrent;

                let ep = email_provider.clone();
                let sys = tick_job_config(&TickConfigSource {
                    infra: &infra,
                    priority_decay,
                    queue_concurrency: &queue_concurrency,
                    queue_timeouts: &queue_timeouts,
                    storage: &storage,
                    lua_infra: &job_lua_infra,
                    job_tasks: &job_tasks,
                });

                // Tracked like the jobs it spawns: a poll caught mid-claim by
                // the shutdown must finish handing its claimed runs to the
                // tracker before the drain decides what to wait for.
                job_tasks.spawn(async move {
                    if let Err(e) = poll_and_execute(
                        &pool, &hook_runner, &registry, max_concurrent, &running_jobs, ep.as_ref(), &sys,
                    ) {
                        error!("Scheduler poll error: {}", e);
                    }
                    // Release the single-flight guard once this poll finishes.
                    poll_in_flight.store(false, Ordering::SeqCst);
                });
            }
            _ = cron_ticker.tick() => {
                let now = Utc::now();

                // Only advance `last_cron_check` when the tick SUCCEEDS. The
                // whole tick runs in one transaction; a transient failure
                // rolls back every slug's enqueue for the window `(last_check,
                // now]`, so advancing unconditionally would silently drop all
                // cron jobs due in that window — the next tick must re-cover it.
                match check_cron_schedules(&pool, &registry, last_cron_check, now, &queue_retries) {
                    Ok(()) => last_cron_check = now,
                    Err(e) => error!("Scheduler cron error (window will be retried): {}", e),
                }

                purge_counter += 1;

                run_periodic_purges(&PurgeTickInput {
                    counter: purge_counter,
                    auto_purge_secs,
                    cron_interval_secs: i64::try_from(config.cron_interval).unwrap_or(i64::MAX),
                    pool: &pool,
                    registry: &registry,
                    storage: &storage,
                    locale_config: &locale_config,
                });
            }
            _ = heartbeat_ticker.tick() => {
                // Refresh this node's own running-job heartbeats first, then
                // reclaim any DEAD peer's jobs (heartbeat expired past the
                // threshold) — this is the runtime half of the at-least-once
                // recovery, so a crashed worker's jobs don't wait for that node
                // to restart.
                update_heartbeats(&pool, &running_jobs);

                if let Ok(conn) = pool.get()
                    && let Err(e) = recover_stale_jobs(&conn, &registry, stale_threshold_secs)
                {
                    error!("Scheduler stale-recovery error: {}", e);
                }
            }
            // Image conversion now runs through the unified job queue as
            // `_system_image_convert` system jobs — see
            // `runner::execute_system_image_convert`. The poll_ticker arm
            // above picks them up alongside email and Lua-handler jobs.
            // No dedicated image ticker / single-flight gate needed; the
            // job queue's `max_concurrent` + per-slug `job_concurrency`
            // map handle worker throttling.
        }
    }
}

/// Event transport + populate cache for user job handlers' Lua CRUD calls,
/// so job writes publish live-update events and invalidate the populate
/// cache like every other surface. The event queue is injected per
/// invocation by `run_job_handler` (which flushes it post-handler).
fn job_crud_infra(infra: &AppInfra) -> LuaCrudInfra {
    LuaCrudInfra::for_pool_crud(infra)
}

/// Recover stale jobs on startup. (Image queue recovery is now handled
/// by `recover_stale_jobs` because image conversion lives in the
/// unified job queue as `_system_image_convert` jobs — see the
/// alpha.9 image-queue → job-queue harmonization in CHANGELOG.)
#[cfg(not(tarpaulin_include))]
fn recover_on_startup(pool: &DbPool, registry: &Registry, stale_threshold_secs: u64) -> Result<()> {
    let conn = pool
        .get()
        .context("Scheduler: failed to get DB connection for recovery")?;

    recover_stale_jobs(&conn, registry, stale_threshold_secs)?;

    Ok(())
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

    let queue_retries = config
        .queues
        .iter()
        .filter_map(|(name, q)| q.retries.map(|r| (name.clone(), r)))
        .collect();

    QueueMaps {
        queue_concurrency,
        queue_timeouts,
        queue_retries,
    }
}

/// Bundle of refs needed by the periodic-purge tick. Bundled into a
/// struct so the call site reads at a glance instead of counting
/// positional arguments.
#[cfg(not(tarpaulin_include))]
struct PurgeTickInput<'a> {
    counter: u64,
    auto_purge_secs: Option<u64>,
    cron_interval_secs: i64,
    pool: &'a DbPool,
    registry: &'a Registry,
    storage: &'a SharedStorage,
    locale_config: &'a LocaleConfig,
}

/// Run periodic purges (every 10 cron intervals).
///
/// In multi-node deployments the retention purge is gated by an atomic
/// `_crap_cron_fired` claim -- only one node runs the purge per cron window.
#[cfg(not(tarpaulin_include))]
fn run_periodic_purges(p: &PurgeTickInput<'_>) {
    if !p.counter.is_multiple_of(10) {
        return;
    }

    let Ok(mut conn) = p.pool.write() else {
        return;
    };

    // The purge fires every 10 cron intervals, so the dedup window must cover
    // that span -- otherwise two nodes drifting by ~1 cron tick would each
    // claim a fresh window and run the purge twice.
    let purge_window_secs = p.cron_interval_secs.saturating_mul(10);

    // `transaction_immediate()` — `claim_retention_purge_tick` runs SELECTs
    // and an INSERT/UPDATE on `_crap_cron_fired`. Same `SQLITE_BUSY_SNAPSHOT`
    // hazard if a concurrent writer commits between the read and write.
    let claimed = match conn.transaction_immediate() {
        Ok(tx) => match claim_retention_purge_tick(&tx, Utc::now(), purge_window_secs) {
            Ok(true) => match tx.commit() {
                Ok(()) => true,
                Err(e) => {
                    warn!("Failed to commit retention-purge claim: {}", e);
                    false
                }
            },
            Ok(false) => {
                debug!("Retention purge already claimed by another instance this window");
                false
            }
            Err(e) => {
                warn!("Retention-purge claim error: {}", e);
                false
            }
        },
        Err(e) => {
            warn!(
                "Failed to open transaction for retention-purge claim: {}",
                e
            );
            false
        }
    };

    if !claimed {
        return;
    }

    // Job-row retention: gated behind the same single-winner claim as the
    // soft-delete purge (was previously run on every node — redundant work).
    if let Some(secs) = p.auto_purge_secs {
        match job_query::purge_old_jobs(&conn, secs) {
            Ok(n) if n > 0 => info!("Auto-purged {} old job run(s)", n),
            Ok(_) => {}
            Err(e) => warn!("Auto-purge error: {}", e),
        }
    }

    // Delete upload files only AFTER the purge transaction has committed.
    let keys_to_clean = run_soft_delete_purge(&mut conn, p.registry, p.locale_config);
    delete_storage_keys(&**p.storage, &keys_to_clean);
}

/// Run the soft-delete retention purge in one IMMEDIATE transaction so the
/// per-doc ref-count decrement + delete is atomic, and return the upload
/// field-maps whose files the caller must delete post-commit. Any failure
/// returns an empty list — a rollback leaves orphaned files (safe) rather than
/// DB rows pointing at deleted files (unsafe).
#[cfg(not(tarpaulin_include))]
fn run_soft_delete_purge(
    conn: &mut BoxedConnection,
    registry: &Registry,
    locale_config: &LocaleConfig,
) -> Vec<String> {
    let tx = match conn.transaction_immediate() {
        Ok(tx) => tx,
        Err(e) => {
            warn!("Failed to open transaction for soft-delete purge: {e}");
            return Vec::new();
        }
    };

    let (purged, files) = match purge_soft_deleted(&tx, registry, locale_config) {
        Ok(result) => result,
        Err(e) => {
            warn!("Soft-delete purge error: {e}"); // tx drops → rollback
            return Vec::new();
        }
    };

    if let Err(e) = tx.commit() {
        warn!("Failed to commit soft-delete purge: {e}");
        return Vec::new();
    }

    if purged > 0 {
        info!("Purged {purged} expired soft-deleted doc(s)");
    }
    files
}

/// Update heartbeats for all currently running jobs.
#[cfg(not(tarpaulin_include))]
fn update_heartbeats(pool: &DbPool, running_jobs: &Arc<Mutex<Vec<String>>>) {
    let ids: Vec<String> = running_jobs
        .lock()
        .map(|guard| guard.clone())
        .unwrap_or_default();

    if ids.is_empty() {
        return;
    }

    let Ok(conn) = pool.get() else { return };

    for id in &ids {
        if let Err(e) = job_query::update_heartbeat(&conn, id) {
            warn!("Heartbeat update error for {}: {}", id, e);
        }
    }
}

/// Log the scheduler's startup line, warn about unused queue config, and
/// reclaim jobs left `running` by a previous process.
///
/// # Errors
///
/// Propagates a stale-job recovery failure.
#[cfg(not(tarpaulin_include))]
fn announce_and_recover(
    config: &JobsConfig,
    pool: &DbPool,
    registry: &Registry,
    stale_threshold_secs: u64,
) -> Result<()> {
    info!(
        "Scheduler started (poll={}s, cron={}s, max_concurrent={})",
        config.poll_interval, config.cron_interval, config.max_concurrent
    );

    warn_unused_queue_config(config, registry);

    recover_on_startup(pool, registry, stale_threshold_secs)
}

/// Borrowed sources for one [`TickJobConfig`] snapshot.
struct TickConfigSource<'a> {
    infra: &'a Arc<AppInfra>,
    priority_decay: u64,
    queue_concurrency: &'a HashMap<String, u32>,
    queue_timeouts: &'a HashMap<String, u64>,
    storage: &'a SharedStorage,
    lua_infra: &'a LuaCrudInfra,
    job_tasks: &'a TaskTracker,
}

/// Snapshot the per-tick execution config (cheap clones) handed to the
/// spawned poll task.
fn tick_job_config(s: &TickConfigSource<'_>) -> TickJobConfig {
    TickJobConfig {
        app_infra: Arc::clone(s.infra),
        priority_decay: s.priority_decay,
        queue_concurrency: s.queue_concurrency.clone(),
        queue_timeouts: s.queue_timeouts.clone(),
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
/// its own timeout, and the outer per-job timer will stamp it `failed`.
async fn drain_job_tasks(
    job_tasks: &TaskTracker,
    running_jobs: &Arc<Mutex<Vec<String>>>,
    deadline: Duration,
) {
    job_tasks.close();

    if job_tasks.is_empty() {
        return;
    }

    let ids: Vec<String> = running_jobs
        .lock()
        .map(|guard| guard.clone())
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

/// Poll for pending jobs and execute them.
#[cfg(not(tarpaulin_include))]
fn poll_and_execute(
    pool: &DbPool,
    hook_runner: &HookRunner,
    registry: &Registry,
    max_concurrent: usize,
    running_jobs: &Arc<Mutex<Vec<String>>>,
    email_provider: Option<&SharedEmailProvider>,
    system: &TickJobConfig,
) -> Result<()> {
    // The write pool: `claim_pending_jobs` below opens an IMMEDIATE transaction
    // on this connection, and a write transaction on a read connection starves
    // concurrent readers.
    let mut conn = pool.write().context("Failed to get DB connection")?;

    let total_running = job_query::count_running(&conn, None)?;
    // Saturate to max_concurrent so a runaway counter still gates new jobs
    // (zero `available` = skip this tick rather than over-claiming).
    let running_usize = usize::try_from(total_running).unwrap_or(max_concurrent);
    if running_usize >= max_concurrent {
        return Ok(());
    }

    let available = max_concurrent - running_usize;
    let job_concurrency = read_job_concurrency(registry);

    let claimed = claim_pending_jobs(
        &mut conn,
        available,
        &job_concurrency,
        &system.queue_concurrency,
        system.priority_decay,
    )?;
    drop(conn);

    for job_run in claimed {
        let Some(job_def) = resolve_job_def(registry, &job_run, pool, &system.queue_timeouts)
        else {
            continue;
        };

        spawn_job_execution(&SpawnJobInput {
            pool,
            hook_runner,
            running_jobs,
            email_provider,
            storage: &system.storage,
            job_run: &job_run,
            job_def: &job_def,
            lua_infra: &system.lua_infra,
            app_infra: &system.app_infra,
            job_tasks: &system.job_tasks,
        });
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

/// Queue names that the framework seeds via
/// `JobsConfig::apply_queue_defaults` even if the operator doesn't
/// declare them. We skip these in [`warn_unused_queue_config`]
/// because a "no job uses queue X" warning would be a false positive:
/// these queues host **system jobs** (`_system_image_convert`,
/// `_system_email`, `_system_bulk`) which live outside `registry.jobs` (they're
/// inserted directly by Rust without a `crap.jobs.define(...)`
/// call), so the registry never reports a user job in them even when
/// the queue is actively in use.
///
/// Keep in sync with the seeding logic in
/// [`JobsConfig::apply_queue_defaults`] and the system-job slug list
/// in `core::job::system::SYSTEM_JOB_SLUGS`.
const FRAMEWORK_DEFAULT_QUEUES: &[&str] = &["images", "email", "bulk"];

/// Warn (don't error) if `[jobs.queues]` references a queue name that
/// no defined job uses. Catches operator typos like
/// `[jobs.queues.mailings] concurrency = 4` when the real queue is
/// `emails`. Framework-seeded defaults (see `FRAMEWORK_DEFAULT_QUEUES`)
/// are excluded to avoid false positives.
#[cfg(not(tarpaulin_include))]
fn warn_unused_queue_config(config: &JobsConfig, registry: &Registry) {
    let known_queues: HashSet<&str> = registry
        .jobs
        .values()
        .map(|def| def.queue.as_str())
        .collect();

    for name in config.queues.keys() {
        if FRAMEWORK_DEFAULT_QUEUES.contains(&name.as_str()) {
            continue;
        }
        if !known_queues.contains(name.as_str()) {
            tracing::warn!(
                "[jobs.queues.{name}] is configured but no defined job uses queue '{name}' — \
                 check for a typo in `crap.toml` or `crap.jobs.define`"
            );
        }
    }
}

/// Claim pending jobs, using IMMEDIATE transaction for `SQLite`.
#[cfg(not(tarpaulin_include))]
fn claim_pending_jobs(
    conn: &mut BoxedConnection,
    available: usize,
    job_concurrency: &HashMap<String, u32>,
    queue_concurrency: &HashMap<String, u32>,
    decay_secs: u64,
) -> Result<Vec<JobRun>> {
    // One transaction path for BOTH backends: the
    // `FOR UPDATE SKIP LOCKED` row locks (Postgres) and the IMMEDIATE
    // write lock (SQLite) must be held across the whole select-count-claim
    // sequence, or the per-slug/per-queue concurrency caps are only
    // advisory across concurrent claimers. Postgres previously ran the
    // claim on a bare autocommit connection, releasing each statement's
    // locks immediately — so two nodes could each claim past a
    // `concurrency = 1` cap in the same tick. `transaction_immediate` is
    // plain BEGIN on Postgres (MVCC needs no IMMEDIATE) and IMMEDIATE on
    // SQLite.
    let tx = conn
        .transaction_immediate()
        .context("Failed to start claim transaction")?;
    let result = job_query::claim_pending_jobs(
        &tx,
        available,
        job_concurrency,
        queue_concurrency,
        decay_secs,
    )?;
    tx.commit().context("Failed to commit claim transaction")?;
    Ok(result)
}

/// A `running` job is treated as dead (its worker stopped heartbeating) once
/// its `heartbeat_at` is older than `heartbeat_interval * this`. Must be > 1
/// so a single missed heartbeat tick doesn't reclaim a live job.
const STALE_HEARTBEAT_MULTIPLIER: u64 = 3;

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

    // `_system_email` and `_system_image_convert` are dispatched by
    // `execute_job` directly (no Lua VM). Synthesize minimal
    // `JobDefinition`s so the scheduler can flow them through the
    // standard claim/execute path. Per-queue concurrency throttling
    // is handled via `[jobs.queues.<queue>] concurrency = N`.
    if job_run.slug == SYSTEM_EMAIL_JOB {
        let timeout = queue_timeouts
            .get(SYSTEM_EMAIL_QUEUE)
            .copied()
            .unwrap_or(DEFAULT_EMAIL_QUEUE_TIMEOUT_SECS);
        return Some(Arc::new(
            JobDefinition::builder(SYSTEM_EMAIL_JOB, "_system")
                .queue(SYSTEM_EMAIL_QUEUE)
                .timeout(timeout)
                .build(),
        ));
    }

    if job_run.slug == SYSTEM_IMAGE_CONVERT_JOB {
        let timeout = queue_timeouts
            .get(IMAGE_CONVERT_QUEUE)
            .copied()
            .unwrap_or(DEFAULT_IMAGES_QUEUE_TIMEOUT_SECS);
        return Some(Arc::new(
            JobDefinition::builder(SYSTEM_IMAGE_CONVERT_JOB, "_system")
                .queue(IMAGE_CONVERT_QUEUE)
                .timeout(timeout)
                .build(),
        ));
    }

    if job_run.slug == SYSTEM_BULK_JOB {
        let timeout = queue_timeouts
            .get(SYSTEM_BULK_QUEUE)
            .copied()
            .unwrap_or(DEFAULT_BULK_QUEUE_TIMEOUT_SECS);
        return Some(Arc::new(
            JobDefinition::builder(SYSTEM_BULK_JOB, "_system")
                .queue(SYSTEM_BULK_QUEUE)
                .timeout(timeout)
                .build(),
        ));
    }

    warn!(
        "Job definition '{}' not found, marking as failed",
        job_run.slug
    );

    if let Ok(c) = pool.get() {
        let _ = job_query::fail_job(
            &c,
            &job_run.id,
            "job definition not found",
            false,
            job_run.attempt,
        )
        .inspect_err(|e| warn!("Failed to mark job {} as failed: {e}", job_run.id));
    }

    None
}

struct SpawnJobInput<'a> {
    pool: &'a DbPool,
    hook_runner: &'a HookRunner,
    running_jobs: &'a Arc<Mutex<Vec<String>>>,
    email_provider: Option<&'a SharedEmailProvider>,
    storage: &'a SharedStorage,
    job_run: &'a JobRun,
    job_def: &'a JobDefinition,
    lua_infra: &'a LuaCrudInfra,
    app_infra: &'a Arc<AppInfra>,
    job_tasks: &'a TaskTracker,
}

/// Spawn a tokio task to execute a job with timeout enforcement.
#[cfg(not(tarpaulin_include))]
fn spawn_job_execution(s: &SpawnJobInput<'_>) {
    if let Ok(mut guard) = s.running_jobs.lock() {
        guard.push(s.job_run.id.clone());
    }

    let pool = s.pool.clone();
    let hook_runner = s.hook_runner.clone();
    let running_jobs = s.running_jobs.clone();
    // `_system_bulk` enforces its OWN cooperative deadline (it aborts and
    // rolls back at `timeout`). Give the uncancellable outer timer extra
    // grace so it can only ever fire for a genuinely stuck run — otherwise
    // it could stamp `failed` while the batch is still doing post-commit
    // work (event publishing, upload-file deletes) and then commits.
    let timeout_secs = if s.job_def.slug.as_ref() == SYSTEM_BULK_JOB {
        s.job_def
            .timeout
            .saturating_add(SELF_LIMITING_JOB_GRACE_SECS)
    } else {
        s.job_def.timeout
    };
    let should_retry = s.job_run.attempt < s.job_run.max_attempts;
    // A bulk run failed here drops its request payload like every other
    // terminal bulk run.
    let bulk_run = (s.job_run.slug == SYSTEM_BULK_JOB).then(|| s.job_run.clone());
    let attempt = s.job_run.attempt;
    let pool_timeout = pool.clone();
    let job_id = s.job_run.id.clone();
    let id_log = s.job_run.id.clone();
    let slug_log = s.job_run.slug.clone();
    let ep = s.email_provider.cloned();
    let storage = s.storage.clone();
    let job_def = s.job_def.clone();
    let job_run = s.job_run.clone();
    let lua_infra = s.lua_infra.clone();
    let app_infra = Arc::clone(s.app_infra);

    // Tracked, not detached: a shutdown waits for this run rather than
    // dropping it mid-transaction.
    s.job_tasks.spawn(async move {
        let timeout_dur = Duration::from_secs(timeout_secs);
        let result = timeout(
            timeout_dur,
            tokio::task::spawn_blocking(move || {
                execute_job(ExecuteJobParams {
                    pool: &pool,
                    hook_runner: &hook_runner,
                    job_def: &job_def,
                    job_run: &job_run,
                    email_provider: ep.as_deref(),
                    storage: &storage,
                    lua_infra: Some(&lua_infra),
                    app_infra: Some(&app_infra),
                })
            }),
        )
        .await;

        if let Ok(mut guard) = running_jobs.lock() {
            guard.retain(|id| id != &job_id);
        }

        // On any non-clean outcome, transition the row out of `running` via a
        // guarded `fail_job` (compare-and-set on running+attempt). `execute_job`
        // writes a terminal status itself on the normal success/handler-error
        // paths and returns `Ok(())`; it returns `Err` ONLY from an early path
        // that ran before writing a terminal status (missing provider, bad
        // job data, a post-handler pool.get failure), and a panic leaves the
        // row `running` too — without this, those jobs stick in `running`
        // forever, permanently consuming a concurrency slot.
        let fail_reason = match result {
            Ok(Ok(Ok(()))) => None,
            Ok(Ok(Err(e))) => {
                error!("Job {} ({}) execution error: {}", id_log, slug_log, e);
                Some(format!("execution error: {e}"))
            }
            Ok(Err(e)) => {
                error!("Job {} ({}) panicked: {}", id_log, slug_log, e);
                Some(format!("handler panicked: {e}"))
            }
            Err(_) => {
                error!(
                    "Job {} ({}) timed out after {}s",
                    id_log, slug_log, timeout_secs
                );
                Some(format!("timeout after {timeout_secs}s"))
            }
        };

        if let Some(reason) = fail_reason
            && let Ok(c) = pool_timeout.get()
        {
            let _ = job_query::fail_job(&c, &id_log, &reason, should_retry, attempt)
                .inspect_err(|e| warn!("Failed to mark job {id_log} as failed: {e}"));

            if let Some(run) = bulk_run.as_ref().filter(|_| !should_retry) {
                strip_finished_payload(&c, run);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use tokio::time::sleep;

    use crate::config::{JobsConfig, QueueConfig};

    use super::*;

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

        let running_jobs = Arc::new(Mutex::new(vec!["job-1".to_string()]));

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
        job_tasks.spawn(std::future::pending::<()>());

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
