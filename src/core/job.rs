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
