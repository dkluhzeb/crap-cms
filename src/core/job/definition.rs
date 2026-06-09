use crate::core::{HookRef, Slug, job::JobLabels};

/// A job definition registered via `crap.jobs.define()` in Lua.
#[derive(Debug, Clone)]
pub struct JobDefinition {
    /// Unique identifier for this job type.
    pub slug: Slug,
    /// Lua function reference for the job handler (e.g., "jobs.cleanup.run").
    /// May carry per-definition options exposed to the handler as `ctx.options`.
    pub handler: HookRef,
    /// Optional cron schedule expression (e.g., "0 3 * * *").
    pub schedule: Option<String>,
    /// Queue name for grouping jobs. Default: "default".
    pub queue: String,
    /// Maximum retry attempts on failure, when set explicitly at
    /// `crap.jobs.define` time. `None` = inherit from
    /// `[jobs.queues.<queue>] retries`; falls back to `0` (no retries)
    /// if neither the definition nor the queue config supplies a
    /// value. Resolve via [`Self::effective_max_attempts`] at queue
    /// time — never compute `retries + 1` directly.
    pub retries: Option<u32>,
    /// Timeout in seconds before a running job is marked failed. Default: 60.
    pub timeout: u64,
    /// Maximum concurrent runs of this specific job. Default: 1.
    pub concurrency: u32,
    /// Default scheduling priority applied when a queue site doesn't
    /// pass an explicit value (cron-fired runs, gRPC trigger without
    /// `priority`, Lua `crap.jobs.queue` without `{ priority = … }`).
    /// Higher = sooner; negative = run only when otherwise idle.
    /// Default: 0.
    pub priority: i32,
    /// Skip scheduled run if a previous run is still running. Default: true.
    pub skip_if_running: bool,
    /// Display labels for admin UI.
    pub labels: JobLabels,
    /// Optional Lua function ref for access control on trigger.
    pub access: Option<HookRef>,
}

impl JobDefinition {
    pub fn builder(slug: impl Into<Slug>, handler: impl Into<HookRef>) -> JobDefinitionBuilder {
        JobDefinitionBuilder::new(slug, handler)
    }

    /// Total attempts (initial + retries) for a job in this definition.
    /// Resolution order: explicit `JobDefinition.retries` wins, then
    /// the queue's `[jobs.queues.<queue>] retries`, then `0` (one
    /// attempt, no retries).
    ///
    /// `queue_retries` is the operator's `Option<u32>` from
    /// `JobsConfig.queues.get(&self.queue).and_then(|q| q.retries)` —
    /// pass `None` from contexts that don't have config access (the
    /// definition's `retries` still applies; the fallback is `0`).
    #[must_use]
    pub fn effective_max_attempts(&self, queue_retries: Option<u32>) -> u32 {
        self.retries
            .or(queue_retries)
            .unwrap_or(0)
            .saturating_add(1)
    }
}

impl Default for JobDefinition {
    fn default() -> Self {
        Self {
            slug: Slug::new(""),
            handler: HookRef::new(""),
            schedule: None,
            queue: "default".to_string(),
            retries: None,
            timeout: 60,
            concurrency: 1,
            priority: 0,
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
    pub fn new(slug: impl Into<Slug>, handler: impl Into<HookRef>) -> Self {
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

    /// Set an explicit retry count. Marks this `JobDefinition` as
    /// "operator chose N retries", overriding any per-queue default.
    /// To leave the field unset (inherit from `[jobs.queues.<queue>]
    /// retries`), simply don't call this method.
    #[must_use]
    pub fn retries(mut self, n: u32) -> Self {
        self.inner.retries = Some(n);

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
    pub fn priority(mut self, p: i32) -> Self {
        self.inner.priority = p;

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
    pub fn access(mut self, a: impl Into<HookRef>) -> Self {
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
        assert_eq!(def.retries, None);
        assert_eq!(def.effective_max_attempts(None), 1);
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
        assert_eq!(def.handler.reference(), "jobs.cleanup.run");
        assert_eq!(def.queue, "default");
        assert_eq!(def.retries, None);
        assert_eq!(def.effective_max_attempts(None), 1);
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
        assert_eq!(def.retries, Some(3));
        assert_eq!(def.timeout, 120);
        assert_eq!(def.concurrency, 2);
        assert!(!def.skip_if_running);
        assert_eq!(
            def.access.as_ref().map(HookRef::reference),
            Some("access.admin_only")
        );
    }

    #[test]
    fn effective_max_attempts_resolves_in_priority_order() {
        // Explicit definition retries wins over queue default.
        let def = JobDefinitionBuilder::new("x", "x.run").retries(5).build();
        assert_eq!(def.effective_max_attempts(Some(2)), 6);
        assert_eq!(def.effective_max_attempts(None), 6);

        // Operator-set retries=0 means "no retries" — beats queue default.
        let def_zero = JobDefinitionBuilder::new("x", "x.run").retries(0).build();
        assert_eq!(def_zero.effective_max_attempts(Some(7)), 1);

        // Unset definition inherits the queue default.
        let def_unset = JobDefinitionBuilder::new("x", "x.run").build();
        assert_eq!(def_unset.effective_max_attempts(Some(3)), 4);

        // Neither set → 1 attempt (no retries).
        assert_eq!(def_unset.effective_max_attempts(None), 1);
    }
}
