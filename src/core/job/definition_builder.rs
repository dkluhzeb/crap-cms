//! Builders for `crate::core::job::JobDefinition` and `crate::core::job::JobRun`.

use crate::core::{
    Slug,
    job::{JobDefinition, JobLabels, JobRun, JobStatus},
};

/// Builder for [`JobDefinition`].
///
/// `slug` and `handler` are taken in `new()`. All other fields default via
/// [`JobDefinition::default()`].
pub struct JobDefinitionBuilder {
    inner: JobDefinition,
}

impl JobDefinitionBuilder {
    pub fn new(slug: impl Into<Slug>, handler: impl Into<String>) -> Self {
        Self {
            inner: JobDefinition {
                slug: slug.into(),
                handler: handler.into(),
                ..Default::default()
            },
        }
    }

    pub fn schedule(mut self, s: impl Into<String>) -> Self {
        self.inner.schedule = Some(s.into());
        self
    }

    pub fn queue(mut self, q: impl Into<String>) -> Self {
        self.inner.queue = q.into();
        self
    }

    pub fn retries(mut self, n: u32) -> Self {
        self.inner.retries = n;
        self
    }

    pub fn timeout(mut self, t: u64) -> Self {
        self.inner.timeout = t;
        self
    }

    pub fn concurrency(mut self, n: u32) -> Self {
        self.inner.concurrency = n;
        self
    }

    pub fn skip_if_running(mut self, b: bool) -> Self {
        self.inner.skip_if_running = b;
        self
    }

    pub fn labels(mut self, l: JobLabels) -> Self {
        self.inner.labels = l;
        self
    }

    pub fn access(mut self, a: impl Into<String>) -> Self {
        self.inner.access = Some(a.into());
        self
    }

    pub fn build(self) -> JobDefinition {
        self.inner
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
    scheduled_by: Option<String>,
    created_at: Option<String>,
    started_at: Option<String>,
    completed_at: Option<String>,
    heartbeat_at: Option<String>,
    retry_after: Option<String>,
}

impl JobRunBuilder {
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
            scheduled_by: None,
            created_at: None,
            started_at: None,
            completed_at: None,
            heartbeat_at: None,
            retry_after: None,
        }
    }

    pub fn status(mut self, s: JobStatus) -> Self {
        self.status = s;
        self
    }

    pub fn queue(mut self, q: impl Into<String>) -> Self {
        self.queue = q.into();
        self
    }

    pub fn data(mut self, d: impl Into<String>) -> Self {
        self.data = d.into();
        self
    }

    pub fn result(mut self, r: impl Into<String>) -> Self {
        self.result = Some(r.into());
        self
    }

    pub fn error(mut self, e: impl Into<String>) -> Self {
        self.error = Some(e.into());
        self
    }

    pub fn attempt(mut self, a: u32) -> Self {
        self.attempt = a;
        self
    }

    pub fn max_attempts(mut self, m: u32) -> Self {
        self.max_attempts = m;
        self
    }

    pub fn scheduled_by(mut self, s: impl Into<String>) -> Self {
        self.scheduled_by = Some(s.into());
        self
    }

    pub fn created_at(mut self, ts: impl Into<String>) -> Self {
        self.created_at = Some(ts.into());
        self
    }

    pub fn started_at(mut self, ts: impl Into<String>) -> Self {
        self.started_at = Some(ts.into());
        self
    }

    pub fn completed_at(mut self, ts: impl Into<String>) -> Self {
        self.completed_at = Some(ts.into());
        self
    }

    pub fn heartbeat_at(mut self, ts: impl Into<String>) -> Self {
        self.heartbeat_at = Some(ts.into());
        self
    }

    pub fn retry_after(mut self, ts: impl Into<String>) -> Self {
        self.retry_after = Some(ts.into());
        self
    }

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
    fn builds_job_definition_with_defaults() {
        let def = JobDefinitionBuilder::new("cleanup", "jobs.cleanup.run").build();
        assert_eq!(def.slug, "cleanup");
        assert_eq!(def.handler, "jobs.cleanup.run");
        assert_eq!(def.queue, "default");
        assert_eq!(def.retries, 0);
        assert_eq!(def.timeout, 60);
        assert_eq!(def.concurrency, 1);
        assert!(def.skip_if_running);
        assert!(def.schedule.is_none());
        assert!(def.access.is_none());
    }

    #[test]
    fn builds_job_definition_with_overrides() {
        let def = JobDefinitionBuilder::new("report", "jobs.report.run")
            .schedule("0 3 * * *")
            .queue("reports")
            .retries(3)
            .timeout(120)
            .concurrency(2)
            .skip_if_running(false)
            .access("access.admin_only")
            .build();
        assert_eq!(def.schedule.as_deref(), Some("0 3 * * *"));
        assert_eq!(def.queue, "reports");
        assert_eq!(def.retries, 3);
        assert_eq!(def.timeout, 120);
        assert_eq!(def.concurrency, 2);
        assert!(!def.skip_if_running);
        assert_eq!(def.access.as_deref(), Some("access.admin_only"));
    }

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
