use serde::{Deserialize, Serialize};

use super::definition_builder::JobRunBuilder;
use super::status::JobStatus;

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

impl JobRun {
    pub fn builder(id: impl Into<String>, slug: impl Into<String>) -> JobRunBuilder {
        JobRunBuilder::new(id, slug)
    }
}
