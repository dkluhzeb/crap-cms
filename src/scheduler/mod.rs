//! Background job scheduler. Polls for pending jobs, evaluates cron
//! schedules, executes Lua handlers, manages heartbeats, and recovers
//! stale jobs across restarts.
//!
//! ## Submodule layout
//!
//! - `loop_runner.rs` -- the long-running event loop (`scheduler::start`).
//!   Owns the tokio `select!` over poll / cron / heartbeat tickers, claims
//!   pending jobs, and spawns timeout-bounded tasks.
//! - `cron_tick.rs` -- the cron tick's blocking body: schedule evaluation
//!   and the periodic retention purge (claim + purge in one transaction).
//! - `heartbeat.rs` -- the heartbeat tick's blocking body: this node's
//!   heartbeats, stale-peer recovery, and the stale threshold they share.
//! - `runner/` -- pure execution helpers: `execute_job` (the Lua
//!   handler / system-email dispatch), `check_cron_schedules`,
//!   `recover_stale_jobs`, `purge_soft_deleted`. No event loop,
//!   no tokio -- callable from tests directly.
//! - `types.rs` -- `SchedulerParams` (call-site struct literal; no
//!   builder ceremony) and the internal `EmailQueueConfig`.
//!
//! ## Conventions
//!
//! - Every tick's database work runs on the blocking pool, never on the
//!   `select!` loop's runtime thread: a heartbeat that waits on the
//!   database must delay neither the shutdown arm nor the other ticks.
//! - Every job-row write takes a write-pool connection.
//! - The retention purge is gated by an atomic `_crap_cron_fired`
//!   claim so multi-node deployments don't double-purge per window.
//! - Job execution runs inside `tokio::time::timeout` +
//!   `spawn_blocking`; on timeout the job is failed via
//!   `job_query::fail_job` with `should_retry` driven by
//!   `attempt < max_attempts`.
//! - Wide-arg helpers take typed `*Input` structs
//!   (`CronTickInput`, `HeartbeatTickInput`, `PurgeCollectionInput`,
//!   `SpawnJobInput`) instead of >4 positional arguments.

mod bulk;
mod cron_tick;
mod heartbeat;
mod loop_runner;
mod runner;
mod types;

pub use loop_runner::start;
pub use runner::{
    ExecuteJobParams, check_cron_schedules, execute_job, purge_soft_deleted, recover_stale_jobs,
};
pub use types::{DbTimeouts, SchedulerParams};

pub(crate) use runner::parse_cron;
