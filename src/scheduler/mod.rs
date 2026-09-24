//! Background job scheduler. Polls for pending jobs, evaluates cron
//! schedules, executes Lua handlers, manages heartbeats, and recovers
//! stale jobs across restarts.
//!
//! ## Submodule layout
//!
//! - `loop_runner.rs` -- the long-running event loop (`scheduler::start`).
//!   Owns the tokio `select!` over the poll / cron / heartbeat tickers and
//!   the run-finished wake-up.
//! - `poll.rs` -- the poll: claims pending jobs up to the free capacity,
//!   executes each claimed run on the blocking pool, and watches it to its
//!   end (a run is never abandoned while it still executes).
//! - `announce.rs` -- the startup announcement: the line stating this
//!   process's effective queues, cron mode and concurrency, the queue-name
//!   typo warnings, and the stale-job recovery that precedes the loop.
//! - `cron_tick.rs` -- the cron tick's blocking body: schedule evaluation
//!   (skipped on a `--no-cron` worker) and the periodic retention purge
//!   (the claim commits with the first bounded purge batch; further batches
//!   follow in their own transactions; run on every worker).
//! - `heartbeat.rs` -- the heartbeat tick's blocking body: this node's
//!   heartbeats, stale-peer recovery, and the stale threshold they share.
//! - `runner/` -- pure execution helpers: `execute_job` (the Lua
//!   handler / system-email dispatch), `check_cron_schedules`,
//!   `recover_stale_jobs`, `purge_soft_deleted`. No event loop,
//!   no tokio -- callable from tests directly.
//! - `types.rs` -- `SchedulerParams` (built via `SchedulerParams::builder`,
//!   whose defaults are `serve`'s: every queue, cron on) and the internal
//!   `TickJobConfig` / `RunningJob` the poll and heartbeat share.
//!
//! ## Conventions
//!
//! - Every tick's database work runs on the blocking pool, never on the
//!   `select!` loop's runtime thread: a heartbeat that waits on the
//!   database must delay neither the shutdown arm nor the other ticks.
//! - Every job-row write takes a write-pool connection.
//! - The retention purge is gated by an atomic `_crap_cron_fired`
//!   claim so multi-node deployments don't double-purge per window.
//! - Job execution runs inside `spawn_blocking`, which Tokio cannot
//!   cancel. A run stops itself at its timeout (Lua handlers and
//!   `_system_bulk` enforce a cooperative deadline and roll back the
//!   operation in flight); the scheduler's timer is only a watchdog that
//!   reports an overdue run and keeps waiting, so the row stays `running`
//!   — and no retry starts — until the run has actually ended.
//! - Every finished run wakes the loop, which polls again at once; the
//!   poll tick is the fallback.
//! - Wide-arg helpers take typed `*Input` structs
//!   (`CronTickInput`, `HeartbeatTickInput`, `PurgeCollectionInput`,
//!   `PollInput`) instead of >4 positional arguments.

mod announce;
mod bulk;
mod cron_tick;
mod heartbeat;
mod loop_runner;
mod poll;
mod runner;
mod types;

pub use loop_runner::start;
pub use runner::{
    ExecuteJobParams, PurgeBatch, RetentionPurge, check_cron_schedules, execute_job,
    purge_soft_deleted, recover_stale_jobs,
};
pub use types::{DbTimeouts, SchedulerParams, SchedulerParamsBuilder};

pub(crate) use runner::parse_cron;
