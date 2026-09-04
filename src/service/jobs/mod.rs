//! Job service layer — access-controlled wrappers around job query functions.
//!
//! The network-reachable paths enforce the job's single access hook
//! (`JobDefinition.access`): `queue_job` with operation `"trigger"`,
//! `list_job_runs`/`get_job_run` with operation `"read"`. See [`access`].
//!
//! [`cancel_pending_jobs`] is the exception — an unguarded admin/CLI primitive
//! (used by `crap-cms jobs cancel`) that takes a raw connection and runs **no**
//! access check. Do not wire it into an untrusted surface without adding a gate.

mod access;
pub mod bulk_queue;
mod cancel;
mod get_run;
mod list_runs;
mod queue;

pub use access::readable_job_slugs;
pub use cancel::{cancel_job_run, cancel_pending_jobs};
pub use get_run::get_job_run;
pub use list_runs::{ListJobRunsInput, list_job_runs};
pub use queue::{QueueJobInput, queue_job};
