use super::definition_builder::JobDefinitionBuilder;
use super::labels::JobLabels;

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

impl JobDefinition {
    pub fn builder(slug: impl Into<String>, handler: impl Into<String>) -> JobDefinitionBuilder {
        JobDefinitionBuilder::new(slug, handler)
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
}
