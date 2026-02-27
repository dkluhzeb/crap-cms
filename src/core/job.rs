//! Job and scheduler types: definitions, runs, and status tracking.

use serde::{Deserialize, Serialize};

/// A job definition registered via `crap.jobs.define()` in Lua.
#[derive(Debug, Clone)]
pub struct JobDefinition {
    /// Unique identifier for this job type.
    pub slug: String,
    /// Lua function reference for the job handler (e.g., "jobs.cleanup.run").
    pub handler: String,
    /// Optional cron schedule expression (e.g., "0 3 * * *").
    pub schedule: Option<String>,
    /// Queue name for grouping jobs. Default: "default".
    pub queue: String,
    /// Maximum retry attempts on failure. Default: 0 (no retries).
    pub retries: u32,
    /// Timeout in seconds before a running job is marked failed. Default: 60.
    pub timeout: u64,
    /// Maximum concurrent runs of this specific job. Default: 1.
    pub concurrency: u32,
    /// Skip scheduled run if a previous run is still running. Default: true.
    pub skip_if_running: bool,
    /// Display labels for admin UI.
    pub labels: JobLabels,
    /// Optional Lua function ref for access control on trigger.
    pub access: Option<String>,
}

/// Display labels for a job definition.
#[derive(Debug, Clone, Default)]
pub struct JobLabels {
    pub singular: Option<String>,
}

/// Status of a job run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Stale,
}

impl JobStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            JobStatus::Pending => "pending",
            JobStatus::Running => "running",
            JobStatus::Completed => "completed",
            JobStatus::Failed => "failed",
            JobStatus::Stale => "stale",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(JobStatus::Pending),
            "running" => Some(JobStatus::Running),
            "completed" => Some(JobStatus::Completed),
            "failed" => Some(JobStatus::Failed),
            "stale" => Some(JobStatus::Stale),
            _ => None,
        }
    }
}

/// A single execution instance of a job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobRun {
    pub id: String,
    pub slug: String,
    pub status: JobStatus,
    pub queue: String,
    /// JSON input data from queue().
    pub data: String,
    /// JSON return value from handler.
    pub result: Option<String>,
    /// Error message if failed.
    pub error: Option<String>,
    pub attempt: u32,
    pub max_attempts: u32,
    /// How this job was triggered: "cron", "manual", "hook", "grpc", "cli".
    pub scheduled_by: Option<String>,
    pub created_at: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub heartbeat_at: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_status_as_str_all_variants() {
        assert_eq!(JobStatus::Pending.as_str(), "pending");
        assert_eq!(JobStatus::Running.as_str(), "running");
        assert_eq!(JobStatus::Completed.as_str(), "completed");
        assert_eq!(JobStatus::Failed.as_str(), "failed");
        assert_eq!(JobStatus::Stale.as_str(), "stale");
    }

    #[test]
    fn job_status_from_str_valid() {
        assert_eq!(JobStatus::from_str("pending"), Some(JobStatus::Pending));
        assert_eq!(JobStatus::from_str("running"), Some(JobStatus::Running));
        assert_eq!(JobStatus::from_str("completed"), Some(JobStatus::Completed));
        assert_eq!(JobStatus::from_str("failed"), Some(JobStatus::Failed));
        assert_eq!(JobStatus::from_str("stale"), Some(JobStatus::Stale));
    }

    #[test]
    fn job_status_from_str_invalid() {
        assert_eq!(JobStatus::from_str("unknown"), None);
        assert_eq!(JobStatus::from_str(""), None);
        assert_eq!(JobStatus::from_str("PENDING"), None);
    }

    #[test]
    fn job_status_roundtrip() {
        for status in &[
            JobStatus::Pending, JobStatus::Running, JobStatus::Completed,
            JobStatus::Failed, JobStatus::Stale,
        ] {
            let s = status.as_str();
            let parsed = JobStatus::from_str(s).expect("should roundtrip");
            assert_eq!(&parsed, status);
        }
    }

    #[test]
    fn job_definition_default() {
        let def = JobDefinition::default();
        assert_eq!(def.queue, "default");
        assert_eq!(def.retries, 0);
        assert_eq!(def.timeout, 60);
        assert_eq!(def.concurrency, 1);
        assert!(def.skip_if_running);
        assert!(def.schedule.is_none());
        assert!(def.access.is_none());
    }
}

impl Default for JobDefinition {
    fn default() -> Self {
        Self {
            slug: String::new(),
            handler: String::new(),
            schedule: None,
            queue: "default".to_string(),
            retries: 0,
            timeout: 60,
            concurrency: 1,
            skip_if_running: true,
            labels: JobLabels::default(),
            access: None,
        }
    }
}
