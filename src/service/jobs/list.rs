//! Listing job definitions — the one description of a defined job.
//!
//! Three surfaces answer "which jobs exist": the gRPC `ListJobs` RPC, the MCP
//! `list_jobs` tool, and `crap-cms jobs list`. They each built their own
//! projection and disagreed about it, so the same job described itself
//! differently depending on where you asked. [`JobDefinitionInfo`] is now the single
//! shape and [`job_definitions`] the single builder.
//!
//! Two entry points, because the callers differ in kind:
//! [`list_jobs`] applies the job access gate (a network caller sees only the
//! jobs whose runs they may read, matching `list_job_runs`), while
//! [`job_definitions`] is the unfiltered operator view the CLI needs — it has no
//! user to gate on.

use std::collections::HashMap;

use crate::{
    core::{Registry, job::JobDefinition},
    db::DbConnection,
    service::{ServiceContext, ServiceError, jobs::readable_job_slugs},
    typegen::lua::LuaAnnotation,
};

/// One defined job: its schedule, queue, and the retry, timeout and
/// concurrency settings it runs with.
// Field for field what the gRPC `JobDefinitionInfo` message carries, so the
// wire projection is a move rather than a translation.
#[derive(Debug, Clone, PartialEq, Eq, LuaAnnotation)]
#[lua(class = "crap.JobDefinitionInfo")]
pub struct JobDefinitionInfo {
    /// The slug that triggers and identifies the job.
    pub slug: String,
    /// Queue the job runs on.
    pub queue: String,
    /// Cron expression, absent for manually triggered jobs.
    #[lua(optional)]
    pub schedule: Option<String>,
    /// Seconds before a running job is considered timed out.
    pub timeout: u64,
    /// Default scheduling priority; higher is claimed sooner.
    pub priority: i32,
    /// Retries after a failure, resolved against the queue's setting.
    pub retries: u32,
    /// Maximum simultaneous runs of this job.
    pub concurrency: u32,
    /// Whether a scheduled run is skipped while another is active.
    pub skip_if_running: bool,
    /// Human-readable label from the Lua definition.
    #[lua(optional)]
    pub label: Option<String>,
}

impl JobDefinitionInfo {
    /// Describe `def`, resolving its retry count against `queue_retries`
    /// (see [`JobsConfig::queue_retries`]).
    ///
    /// [`JobsConfig::queue_retries`]: crate::config::JobsConfig::queue_retries
    #[must_use]
    pub fn describe(def: &JobDefinition, queue_retries: &HashMap<String, u32>) -> Self {
        Self {
            slug: def.slug.to_string(),
            queue: def.queue.clone(),
            schedule: def.schedule.clone(),
            timeout: def.timeout,
            priority: def.priority,
            retries: def.effective_retries(queue_retries.get(&def.queue).copied()),
            concurrency: def.concurrency,
            skip_if_running: def.skip_if_running,
            label: def.labels.singular.clone(),
        }
    }
}

/// Every defined job, in slug order, with no access check.
///
/// The operator view: the CLI runs as whoever holds the config directory and
/// has no user to gate on. Do not wire this into a network surface — use
/// [`list_jobs`].
#[must_use]
pub fn job_definitions(
    registry: &Registry,
    queue_retries: &HashMap<String, u32>,
) -> Vec<JobDefinitionInfo> {
    let mut slugs: Vec<_> = registry.jobs.keys().collect();
    slugs.sort();

    slugs
        .into_iter()
        .map(|slug| JobDefinitionInfo::describe(&registry.jobs[slug], queue_retries))
        .collect()
}

/// The defined jobs `ctx.user` may see, in slug order.
///
/// Visibility is the job's run-read gate: a job whose access hook denies this
/// caller is absent entirely, so a listing never reveals that it exists.
///
/// # Errors
///
/// Returns an error when the access hook fails or returns a filter table
/// (job access is allow/deny only).
pub fn list_jobs(
    ctx: &ServiceContext,
    conn: &dyn DbConnection,
    registry: &Registry,
    queue_retries: &HashMap<String, u32>,
) -> Result<Vec<JobDefinitionInfo>, ServiceError> {
    let mut readable = readable_job_slugs(ctx, conn, registry)?;
    readable.sort();

    Ok(readable
        .iter()
        .filter_map(|slug| registry.get_job(slug))
        .map(|def| JobDefinitionInfo::describe(def, queue_retries))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{HookRef, job::JobDefinition};

    fn job(slug: &str, queue: &str) -> JobDefinition {
        JobDefinition::builder(slug, HookRef::new("jobs.x.run"))
            .queue(queue.to_string())
            .build()
    }

    /// The reported retry count resolves the same way the scheduler resolves
    /// attempts: the definition wins, then the queue, then zero.
    #[test]
    fn retries_resolve_definition_then_queue_then_zero() {
        let queue_retries = HashMap::from([("emails".to_string(), 5)]);

        let mut explicit = job("a", "emails");
        explicit.retries = Some(2);
        assert_eq!(
            JobDefinitionInfo::describe(&explicit, &queue_retries).retries,
            2
        );

        let inherited = job("b", "emails");
        assert_eq!(
            JobDefinitionInfo::describe(&inherited, &queue_retries).retries,
            5
        );

        let unset = job("c", "default");
        assert_eq!(
            JobDefinitionInfo::describe(&unset, &queue_retries).retries,
            0
        );
    }

    /// The reported retry count and the scheduler's attempt count are one
    /// resolution — attempts is always retries plus the first run.
    #[test]
    fn reported_retries_and_scheduled_attempts_agree() {
        let queue_retries = HashMap::from([("emails".to_string(), 5)]);
        let def = job("b", "emails");

        let reported = JobDefinitionInfo::describe(&def, &queue_retries).retries;
        let attempts = def.effective_max_attempts(queue_retries.get(&def.queue).copied());

        assert_eq!(attempts, reported + 1);
    }

    /// Listing order is the slug order, so two calls — and two surfaces —
    /// never present the same jobs in a different sequence.
    #[test]
    fn jobs_are_listed_in_slug_order() {
        let mut registry = Registry::new();
        for slug in ["zeta", "alpha", "mid"] {
            registry.register_job(job(slug, "default"));
        }

        let infos = job_definitions(&registry, &HashMap::new());
        let slugs: Vec<&str> = infos.iter().map(|i| i.slug.as_str()).collect();

        assert_eq!(slugs, ["alpha", "mid", "zeta"]);
    }

    /// Every field a surface renders comes from the definition — a new field
    /// on `JobDefinition` that a surface needs must land here, not in the
    /// surface.
    #[test]
    fn describe_carries_the_whole_definition() {
        let mut def = job("digest", "emails");
        def.schedule = Some("0 3 * * *".to_string());
        def.timeout = 120;
        def.priority = -5;
        def.concurrency = 3;
        def.skip_if_running = false;
        def.labels.singular = Some("Digest".to_string());

        let info = JobDefinitionInfo::describe(&def, &HashMap::new());

        assert_eq!(info.slug, "digest");
        assert_eq!(info.queue, "emails");
        assert_eq!(info.schedule.as_deref(), Some("0 3 * * *"));
        assert_eq!(info.timeout, 120);
        assert_eq!(info.priority, -5);
        assert_eq!(info.concurrency, 3);
        assert!(!info.skip_if_running);
        assert_eq!(info.label.as_deref(), Some("Digest"));
    }
}
