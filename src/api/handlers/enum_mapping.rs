//! Conversions from internal Rust enums to the generated proto enums used
//! on the gRPC wire (`MutationEvent`, `VersionInfo`, `JobRunInfo`).

use crate::api::content;
use crate::core::event::{EventOperation, EventTarget};
use crate::core::{JobStatus, ScheduledBy};

pub(in crate::api::handlers) fn mutation_operation(
    op: &EventOperation,
) -> content::MutationOperation {
    match op {
        EventOperation::Create => content::MutationOperation::Create,
        EventOperation::Update => content::MutationOperation::Update,
        EventOperation::Delete => content::MutationOperation::Delete,
        EventOperation::Undelete => content::MutationOperation::Undelete,
        EventOperation::Unpublish => content::MutationOperation::Unpublish,
        EventOperation::Restore => content::MutationOperation::Restore,
    }
}

pub(in crate::api::handlers) fn mutation_target(target: &EventTarget) -> content::MutationTarget {
    match target {
        EventTarget::Collection => content::MutationTarget::Collection,
        EventTarget::Global => content::MutationTarget::Global,
    }
}

pub(in crate::api::handlers) fn job_run_status(status: JobStatus) -> content::JobRunStatus {
    match status {
        JobStatus::Pending => content::JobRunStatus::Pending,
        JobStatus::Running => content::JobRunStatus::Running,
        JobStatus::Completed => content::JobRunStatus::Completed,
        JobStatus::Failed => content::JobRunStatus::Failed,
        JobStatus::Stale => content::JobRunStatus::Stale,
    }
}

/// The proto enum of a job run's provenance.
fn scheduled_by_proto(by: ScheduledBy) -> content::JobScheduledBy {
    match by {
        ScheduledBy::Grpc => content::JobScheduledBy::Grpc,
        ScheduledBy::Cron => content::JobScheduledBy::Cron,
        ScheduledBy::Hook => content::JobScheduledBy::Hook,
        ScheduledBy::Mcp => content::JobScheduledBy::Mcp,
        ScheduledBy::Cli => content::JobScheduledBy::Cli,
        ScheduledBy::System => content::JobScheduledBy::System,
    }
}

/// Map a run's stored `scheduled_by` to its proto enum, read through
/// [`ScheduledBy::from_stored`] (so a legacy `"api"` row is `Grpc`). A value
/// that names no provenance, or `None`, is `Unspecified`.
pub(in crate::api::handlers) fn job_scheduled_by(value: Option<&str>) -> content::JobScheduledBy {
    value
        .and_then(ScheduledBy::from_stored)
        .map_or(content::JobScheduledBy::Unspecified, scheduled_by_proto)
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
        assert_eq!(
            mutation_operation(&EventOperation::Undelete),
            content::MutationOperation::Undelete
        );
        assert_eq!(
            mutation_operation(&EventOperation::Unpublish),
            content::MutationOperation::Unpublish
        );
        assert_eq!(
            mutation_operation(&EventOperation::Restore),
            content::MutationOperation::Restore
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
            assert_eq!(job_run_status(internal), proto);
        }
    }

    #[test]
    fn job_scheduled_by_maps_every_variant() {
        let cases = [
            (ScheduledBy::Grpc, content::JobScheduledBy::Grpc),
            (ScheduledBy::Cron, content::JobScheduledBy::Cron),
            (ScheduledBy::Hook, content::JobScheduledBy::Hook),
            (ScheduledBy::Mcp, content::JobScheduledBy::Mcp),
            (ScheduledBy::Cli, content::JobScheduledBy::Cli),
            (ScheduledBy::System, content::JobScheduledBy::System),
        ];
        assert_eq!(
            cases.len(),
            ScheduledBy::ALL.len(),
            "every provenance is pinned"
        );

        for (internal, proto) in cases {
            assert_eq!(job_scheduled_by(Some(internal.as_str())), proto);
        }
    }

    /// Regression: an email / image-conversion / migration run (`"system"`)
    /// had no proto value and reported `Unspecified`.
    #[test]
    fn a_system_run_is_reported_as_system() {
        assert_eq!(
            job_scheduled_by(Some("system")),
            content::JobScheduledBy::System
        );
    }

    #[test]
    fn job_scheduled_by_legacy_and_unknown() {
        // Earlier releases recorded every queued bulk op as "api".
        assert_eq!(job_scheduled_by(Some("api")), content::JobScheduledBy::Grpc);
        // Absent or unrecognized → Unspecified.
        assert_eq!(job_scheduled_by(None), content::JobScheduledBy::Unspecified);
        assert_eq!(
            job_scheduled_by(Some("manual")),
            content::JobScheduledBy::Unspecified
        );
        assert_eq!(
            job_scheduled_by(Some("something-else")),
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
            assert_eq!(job_status_filter(job_run_status(status)), Some(status));
        }
    }
}
