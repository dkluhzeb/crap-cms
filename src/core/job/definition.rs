use crate::core::{Slug, job::JobLabels};

/// A job definition registered via `crap.jobs.define()` in Lua.
#[derive(Debug, Clone)]
pub struct JobDefinition {
    /// Unique identifier for this job type.
    pub slug: Slug,
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

impl JobDefinition {
    pub fn builder(slug: impl Into<Slug>, handler: impl Into<String>) -> JobDefinitionBuilder {
        JobDefinitionBuilder::new(slug, handler)
    }
}

impl Default for JobDefinition {
    fn default() -> Self {
        Self {
            slug: Slug::new(""),
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

/// Builder for [`JobDefinition`].
///
/// `slug` and `handler` are taken in `new()`. All other fields default via
/// [`JobDefinition::default()`].
pub struct JobDefinitionBuilder {
    inner: JobDefinition,
}

impl JobDefinitionBuilder {
    /// Create a new builder with the required `slug` and `handler` fields.
    pub fn new(slug: impl Into<Slug>, handler: impl Into<String>) -> Self {
        Self {
            inner: JobDefinition {
                slug: slug.into(),
                handler: handler.into(),
                ..Default::default()
            },
        }
    }

    #[must_use]
    pub fn schedule(mut self, s: impl Into<String>) -> Self {
        self.inner.schedule = Some(s.into());

        self
    }

    #[must_use]
    pub fn queue(mut self, q: impl Into<String>) -> Self {
        self.inner.queue = q.into();

        self
    }

    #[must_use]
    pub fn retries(mut self, n: u32) -> Self {
        self.inner.retries = n;

        self
    }

    #[must_use]
    pub fn timeout(mut self, t: u64) -> Self {
        self.inner.timeout = t;

        self
    }

    #[must_use]
    pub fn concurrency(mut self, n: u32) -> Self {
        self.inner.concurrency = n;

        self
    }

    #[must_use]
    pub fn skip_if_running(mut self, b: bool) -> Self {
        self.inner.skip_if_running = b;

        self
    }

    #[must_use]
    pub fn labels(mut self, l: JobLabels) -> Self {
        self.inner.labels = l;

        self
    }

    #[must_use]
    pub fn access(mut self, a: impl Into<String>) -> Self {
        self.inner.access = Some(a.into());

        self
    }

    /// Build the final [`JobDefinition`].
    #[must_use]
    pub fn build(self) -> JobDefinition {
        self.inner
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
