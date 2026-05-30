//! Conversions from internal Rust enums to the generated proto enums used
//! on the gRPC wire (`MutationEvent`, `VersionInfo`, `JobRunInfo`).

use crate::api::content;
use crate::core::JobStatus;
use crate::core::event::{EventOperation, EventTarget};

pub(in crate::api::handlers) fn mutation_operation(
    op: &EventOperation,
) -> content::MutationOperation {
    match op {
        EventOperation::Create => content::MutationOperation::Create,
        EventOperation::Update => content::MutationOperation::Update,
        EventOperation::Delete => content::MutationOperation::Delete,
    }
}

pub(in crate::api::handlers) fn mutation_target(target: &EventTarget) -> content::MutationTarget {
    match target {
        EventTarget::Collection => content::MutationTarget::Collection,
        EventTarget::Global => content::MutationTarget::Global,
    }
}

pub(in crate::api::handlers) fn job_run_status(status: &JobStatus) -> content::JobRunStatus {
    match status {
        JobStatus::Pending => content::JobRunStatus::Pending,
        JobStatus::Running => content::JobRunStatus::Running,
        JobStatus::Completed => content::JobRunStatus::Completed,
        JobStatus::Failed => content::JobRunStatus::Failed,
        JobStatus::Stale => content::JobRunStatus::Stale,
    }
}

/// Map the stored `scheduled_by` string (`"grpc"`/`"cron"`/`"hook"`, or absent)
/// to its proto enum. Anything else — including `None` — is `Unspecified`.
pub(in crate::api::handlers) fn job_scheduled_by(value: Option<&str>) -> content::JobScheduledBy {
    match value {
        Some("grpc") => content::JobScheduledBy::Grpc,
        Some("cron") => content::JobScheduledBy::Cron,
        Some("hook") => content::JobScheduledBy::Hook,
        _ => content::JobScheduledBy::Unspecified,
    }
}

pub(in crate::api::handlers) fn version_status(value: &str) -> content::VersionStatus {
    match value {
        "published" => content::VersionStatus::Published,
        "draft" => content::VersionStatus::Draft,
        _ => content::VersionStatus::Unspecified,
    }
}

/// Reverse direction: the `ListJobRuns` request status filter. `Unspecified`
/// means "no filter" (all statuses).
pub(in crate::api::handlers) fn job_status_filter(
    status: content::JobRunStatus,
) -> Option<JobStatus> {
    match status {
        content::JobRunStatus::Unspecified => None,
        content::JobRunStatus::Pending => Some(JobStatus::Pending),
        content::JobRunStatus::Running => Some(JobStatus::Running),
        content::JobRunStatus::Completed => Some(JobStatus::Completed),
        content::JobRunStatus::Failed => Some(JobStatus::Failed),
        content::JobRunStatus::Stale => Some(JobStatus::Stale),
    }
}
