use serde::{Deserialize, Serialize};

use crate::core::job::JobStatus;

/// A single execution instance of a job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobRun {
    pub id: String,
    pub slug: String,
    pub status: JobStatus,
    pub queue: String,
    /// JSON input data from `queue()`.
    pub data: String,
    /// JSON return value from handler.
    pub result: Option<String>,
    /// Error message if failed.
    pub error: Option<String>,
    pub attempt: u32,
    pub max_attempts: u32,
    /// Static numeric priority; higher = claimed sooner. Default 0.
    /// Combined with the configured `priority_decay` to produce the
    /// effective scheduling priority in `claim_pending_jobs`.
    pub priority: i32,
    /// Dedup key set by callers passing `{ unique = "..." }` to
    /// `crap.jobs.queue`. `None` for normal enqueues. While the row
    /// is `pending` or `running`, the partial unique index on
    /// `(slug, unique_key)` prevents a second active row with the
    /// same pair from being inserted.
    pub unique_key: Option<String>,
    /// How this job was triggered: "cron", "hook", "grpc", "cli".
    pub scheduled_by: Option<String>,
    pub created_at: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub heartbeat_at: Option<String>,
    /// Earliest time this job can be retried (exponential backoff).
    pub retry_after: Option<String>,
}

impl JobRun {
    pub fn builder(id: impl Into<String>, slug: impl Into<String>) -> JobRunBuilder {
        JobRunBuilder::new(id, slug)
    }
}

/// Builder for [`JobRun`].
///
/// `id` and `slug` are taken in `new()`. Sensible defaults are pre-populated.
pub struct JobRunBuilder {
    id: String,
    slug: String,
    status: JobStatus,
    queue: String,
    data: String,
    result: Option<String>,
    error: Option<String>,
    attempt: u32,
    max_attempts: u32,
    priority: i32,
    unique_key: Option<String>,
    scheduled_by: Option<String>,
    created_at: Option<String>,
    started_at: Option<String>,
    completed_at: Option<String>,
    heartbeat_at: Option<String>,
    retry_after: Option<String>,
}

impl JobRunBuilder {
    /// Create a new builder with the required `id` and `slug` fields.
    pub fn new(id: impl Into<String>, slug: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            slug: slug.into(),
            status: JobStatus::Pending,
            queue: "default".to_string(),
            data: "{}".to_string(),
            result: None,
            error: None,
            attempt: 0,
            max_attempts: 1,
            priority: 0,
            unique_key: None,
            scheduled_by: None,
            created_at: None,
            started_at: None,
            completed_at: None,
            heartbeat_at: None,
            retry_after: None,
        }
    }

    #[must_use]
    pub fn status(mut self, s: JobStatus) -> Self {
        self.status = s;

        self
    }

    #[must_use]
    pub fn queue(mut self, q: impl Into<String>) -> Self {
        self.queue = q.into();

        self
    }

    #[must_use]
    pub fn data(mut self, d: impl Into<String>) -> Self {
        self.data = d.into();

        self
    }

    #[must_use]
    pub fn result(mut self, r: impl Into<String>) -> Self {
        self.result = Some(r.into());

        self
    }

    #[must_use]
    pub fn error(mut self, e: impl Into<String>) -> Self {
        self.error = Some(e.into());

        self
    }

    #[must_use]
    pub fn attempt(mut self, a: u32) -> Self {
        self.attempt = a;

        self
    }

    #[must_use]
    pub fn max_attempts(mut self, m: u32) -> Self {
        self.max_attempts = m;

        self
    }

    #[must_use]
    pub fn priority(mut self, p: i32) -> Self {
        self.priority = p;

        self
    }

    #[must_use]
    pub fn unique_key(mut self, k: impl Into<String>) -> Self {
        self.unique_key = Some(k.into());

        self
    }

    #[must_use]
    pub fn scheduled_by(mut self, s: impl Into<String>) -> Self {
        self.scheduled_by = Some(s.into());

        self
    }

    #[must_use]
    pub fn created_at(mut self, ts: impl Into<String>) -> Self {
        self.created_at = Some(ts.into());

        self
    }

    #[must_use]
    pub fn started_at(mut self, ts: impl Into<String>) -> Self {
        self.started_at = Some(ts.into());

        self
    }

    #[must_use]
    pub fn completed_at(mut self, ts: impl Into<String>) -> Self {
        self.completed_at = Some(ts.into());

        self
    }

    #[must_use]
    pub fn heartbeat_at(mut self, ts: impl Into<String>) -> Self {
        self.heartbeat_at = Some(ts.into());

        self
    }

    #[must_use]
    pub fn retry_after(mut self, ts: impl Into<String>) -> Self {
        self.retry_after = Some(ts.into());

        self
    }

    /// Build the final [`JobRun`].
    #[must_use]
    pub fn build(self) -> JobRun {
        JobRun {
            id: self.id,
            slug: self.slug,
            status: self.status,
            queue: self.queue,
            data: self.data,
            result: self.result,
            error: self.error,
            attempt: self.attempt,
            max_attempts: self.max_attempts,
            priority: self.priority,
            unique_key: self.unique_key,
            scheduled_by: self.scheduled_by,
            created_at: self.created_at,
            started_at: self.started_at,
            completed_at: self.completed_at,
            heartbeat_at: self.heartbeat_at,
            retry_after: self.retry_after,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_job_run_with_defaults() {
        let run = JobRunBuilder::new("run-1", "cleanup").build();
        assert_eq!(run.id, "run-1");
        assert_eq!(run.slug, "cleanup");
        assert_eq!(run.status, JobStatus::Pending);
        assert_eq!(run.queue, "default");
        assert_eq!(run.data, "{}");
        assert_eq!(run.attempt, 0);
        assert_eq!(run.max_attempts, 1);
        assert!(run.result.is_none());
        assert!(run.error.is_none());
        assert!(run.created_at.is_none());
    }

    #[test]
    fn builds_job_run_with_all_fields() {
        let run = JobRunBuilder::new("run-2", "report")
            .status(JobStatus::Completed)
            .queue("reports")
            .data(r#"{"foo":"bar"}"#)
            .result(r#"{"ok":true}"#)
            .error("none")
            .attempt(2)
            .max_attempts(3)
            .scheduled_by("cron")
            .created_at("2024-01-01T00:00:00Z")
            .started_at("2024-01-01T00:01:00Z")
            .completed_at("2024-01-01T00:02:00Z")
            .heartbeat_at("2024-01-01T00:01:30Z")
            .build();
        assert_eq!(run.status, JobStatus::Completed);
        assert_eq!(run.queue, "reports");
        assert_eq!(run.attempt, 2);
        assert_eq!(run.max_attempts, 3);
        assert_eq!(run.scheduled_by.as_deref(), Some("cron"));
        assert_eq!(run.result.as_deref(), Some(r#"{"ok":true}"#));
        assert_eq!(run.completed_at.as_deref(), Some("2024-01-01T00:02:00Z"));
        assert_eq!(run.heartbeat_at.as_deref(), Some("2024-01-01T00:01:30Z"));
    }
}
