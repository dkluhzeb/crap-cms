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

/// Map the stored `scheduled_by` string (`"grpc"`/`"cron"`/`"hook"`) to its
/// proto enum. Anything else — `"cli"` (no proto variant), unknown values, or
/// `None` — is `Unspecified`.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutation_operation_maps_every_variant() {
        assert_eq!(
            mutation_operation(&EventOperation::Create),
            content::MutationOperation::Create
        );
        assert_eq!(
            mutation_operation(&EventOperation::Update),
            content::MutationOperation::Update
        );
        assert_eq!(
            mutation_operation(&EventOperation::Delete),
            content::MutationOperation::Delete
        );
    }

    #[test]
    fn mutation_target_maps_every_variant() {
        assert_eq!(
            mutation_target(&EventTarget::Collection),
            content::MutationTarget::Collection
        );
        assert_eq!(
            mutation_target(&EventTarget::Global),
            content::MutationTarget::Global
        );
    }

    #[test]
    fn job_run_status_maps_every_variant() {
        let cases = [
            (JobStatus::Pending, content::JobRunStatus::Pending),
            (JobStatus::Running, content::JobRunStatus::Running),
            (JobStatus::Completed, content::JobRunStatus::Completed),
            (JobStatus::Failed, content::JobRunStatus::Failed),
            (JobStatus::Stale, content::JobRunStatus::Stale),
        ];
        for (internal, proto) in cases {
            assert_eq!(job_run_status(&internal), proto);
        }
    }

    #[test]
    fn job_scheduled_by_known_and_unknown() {
        assert_eq!(
            job_scheduled_by(Some("grpc")),
            content::JobScheduledBy::Grpc
        );
        assert_eq!(
            job_scheduled_by(Some("cron")),
            content::JobScheduledBy::Cron
        );
        assert_eq!(
            job_scheduled_by(Some("hook")),
            content::JobScheduledBy::Hook
        );
        // Absent or unrecognized → Unspecified.
        assert_eq!(job_scheduled_by(None), content::JobScheduledBy::Unspecified);
        assert_eq!(
            job_scheduled_by(Some("api")),
            content::JobScheduledBy::Unspecified
        );
    }

    #[test]
    fn version_status_known_and_unknown() {
        assert_eq!(
            version_status("published"),
            content::VersionStatus::Published
        );
        assert_eq!(version_status("draft"), content::VersionStatus::Draft);
        assert_eq!(version_status(""), content::VersionStatus::Unspecified);
        assert_eq!(
            version_status("archived"),
            content::VersionStatus::Unspecified
        );
    }

    #[test]
    fn job_status_filter_unspecified_means_no_filter() {
        assert_eq!(job_status_filter(content::JobRunStatus::Unspecified), None);
    }

    /// `job_status_filter` is the inverse of `job_run_status` for every
    /// concrete status — a value round-trips losslessly.
    #[test]
    fn job_status_filter_round_trips_job_run_status() {
        for status in [
            JobStatus::Pending,
            JobStatus::Running,
            JobStatus::Completed,
            JobStatus::Failed,
            JobStatus::Stale,
        ] {
            assert_eq!(job_status_filter(job_run_status(&status)), Some(status));
        }
    }
}
