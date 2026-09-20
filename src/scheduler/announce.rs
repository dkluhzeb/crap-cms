//! The scheduler's startup announcement: the line that states what this
//! process will actually do, the queue-name typo warnings that go with it,
//! and the stale-job recovery it runs before the loop starts.

use std::collections::HashSet;

use anyhow::Result;
use tracing::{info, warn};

use crate::{config::JobsConfig, core::Registry, db::DbPool};

use super::heartbeat::recover_on_startup;

/// Queue names that the framework seeds via
/// `JobsConfig::apply_queue_defaults` even if the operator doesn't
/// declare them. They are part of [`queue_inventory`] because a
/// "no job uses queue X" warning would be a false positive for them:
/// these queues host **system jobs** (`_system_image_convert`,
/// `_system_email`, `_system_bulk`) which live outside `registry.jobs` (they're
/// inserted directly by Rust without a `crap.jobs.define(...)`
/// call), so the registry never reports a user job in them even when
/// the queue is actively in use.
///
/// Keep in sync with the seeding logic in
/// [`JobsConfig::apply_queue_defaults`] and the system-job slug list
/// in `core::job::system::SYSTEM_JOB_SLUGS`.
const FRAMEWORK_DEFAULT_QUEUES: &[&str] = &["images", "email", "bulk"];

/// Every queue name a job can actually land in: the queues of the defined
/// jobs plus the framework's own system queues.
///
/// One inventory behind both "is this `[jobs.queues]` entry a typo" and "is
/// this `--queues` entry a typo", so the two questions can't drift on what
/// counts as a real queue.
fn queue_inventory(registry: &Registry) -> HashSet<&str> {
    registry
        .jobs
        .values()
        .map(|def| def.queue.as_str())
        .chain(FRAMEWORK_DEFAULT_QUEUES.iter().copied())
        .collect()
}

/// The `--queues` entries no defined job and no system job uses. A worker
/// filtered to only those claims nothing at all, silently, so the caller
/// warns about each.
fn unknown_worker_queues<'a>(queues: Option<&'a [String]>, registry: &Registry) -> Vec<&'a str> {
    let Some(queues) = queues else {
        return Vec::new();
    };

    let known = queue_inventory(registry);

    queues
        .iter()
        .map(String::as_str)
        .filter(|name| !known.contains(name))
        .collect()
}

/// Warn (don't error) if `[jobs.queues]` references a queue name that
/// no defined job uses. Catches operator typos like
/// `[jobs.queues.mailings] concurrency = 4` when the real queue is
/// `emails`. Framework-seeded defaults (see `FRAMEWORK_DEFAULT_QUEUES`)
/// are part of the inventory, so they never warn.
#[cfg(not(tarpaulin_include))]
fn warn_unused_queue_config(config: &JobsConfig, registry: &Registry) {
    let known_queues = queue_inventory(registry);

    for name in config.queues.keys() {
        if !known_queues.contains(name.as_str()) {
            warn!(
                "[jobs.queues.{name}] is configured but no defined job uses queue '{name}' — \
                 check for a typo in `crap.toml` or `crap.jobs.define`"
            );
        }
    }
}

/// How the startup line names the queues this process claims from.
fn queues_label(queues: Option<&[String]>) -> String {
    let Some(list) = queues else {
        return "all".to_string();
    };

    if list.is_empty() {
        return "none (empty --queues list)".to_string();
    }

    list.join(",")
}

/// How the startup line names this process's cron mode. A `--no-cron`
/// process still runs the retention purges, so the line says so rather than
/// leaving an operator to conclude retention stopped too.
fn cron_label(config: &JobsConfig, run_cron: bool) -> String {
    if run_cron {
        return format!("{}s", config.cron_interval);
    }

    "off (retention purges only)".to_string()
}

/// What the startup announcement reports, and what the recovery step it runs
/// needs. Built once, at the one call site in `scheduler::start`.
pub(super) struct StartupAnnounce<'a> {
    pub config: &'a JobsConfig,
    pub pool: &'a DbPool,
    pub registry: &'a Registry,
    pub stale_threshold_secs: u64,
    /// The `--queues` allow-list, or `None` for every queue.
    pub queues: Option<&'a [String]>,
    /// Whether this process evaluates cron schedules.
    pub run_cron: bool,
}

/// Log the scheduler's startup line — stating the effective queues and cron
/// mode, so an operator can read a worker's actual scope off its first log
/// line — warn about queue names nothing uses, and reclaim jobs left
/// `running` by a previous process.
///
/// # Errors
///
/// Propagates a stale-job recovery failure.
#[cfg(not(tarpaulin_include))]
pub(super) fn announce_and_recover(a: &StartupAnnounce<'_>) -> Result<()> {
    info!(
        "Scheduler started (poll={}s, cron={}, max_concurrent={}, queues={})",
        a.config.poll_interval,
        cron_label(a.config, a.run_cron),
        a.config.max_concurrent,
        queues_label(a.queues)
    );

    warn_unused_queue_config(a.config, a.registry);

    for name in unknown_worker_queues(a.queues, a.registry) {
        warn!(
            "--queues names '{name}', but no defined job and no system job uses that queue — \
             this worker will never claim anything from it; check for a typo in \
             `crap.jobs.define` or in the flag"
        );
    }

    recover_on_startup(a.pool, a.registry, a.stale_threshold_secs)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::core::{JobDefinition, Slug};

    use super::*;

    /// A registry with one job on `reports`.
    fn registry_with_reports_job() -> Registry {
        let mut registry = Registry::default();
        registry.jobs.insert(
            Slug::new("nightly_report"),
            Arc::new(
                JobDefinition::builder("nightly_report", "reports.nightly")
                    .queue("reports")
                    .build(),
            ),
        );

        registry
    }

    /// `--queues nope` is a typo: nothing enqueues into that queue, so the
    /// worker would sit idle forever without ever saying why.
    #[test]
    fn an_unknown_worker_queue_is_reported() {
        let registry = registry_with_reports_job();
        let queues = vec!["nope".to_string()];

        assert_eq!(
            unknown_worker_queues(Some(&queues), &registry),
            vec!["nope"]
        );
    }

    /// A queue a defined job uses, and every framework system queue, are
    /// real queues — none of them may be reported.
    #[test]
    fn known_and_system_worker_queues_are_not_reported() {
        let registry = registry_with_reports_job();
        let queues = vec![
            "reports".to_string(),
            "images".to_string(),
            "email".to_string(),
            "bulk".to_string(),
        ];

        assert!(unknown_worker_queues(Some(&queues), &registry).is_empty());
    }

    /// Only the unknown entries are reported, not the whole list.
    #[test]
    fn only_the_unknown_worker_queues_are_reported() {
        let registry = registry_with_reports_job();
        let queues = vec![
            "reports".to_string(),
            "reprots".to_string(),
            "images".to_string(),
        ];

        assert_eq!(
            unknown_worker_queues(Some(&queues), &registry),
            vec!["reprots"]
        );
    }

    /// No `--queues` flag means every queue — there is nothing to warn about.
    #[test]
    fn no_worker_queue_filter_reports_nothing() {
        let registry = registry_with_reports_job();

        assert!(unknown_worker_queues(None, &registry).is_empty());
    }

    /// The startup line has to state the effective scope: an operator reads a
    /// worker's queues and cron mode off it.
    #[test]
    fn the_startup_line_states_the_effective_scope() {
        let config = JobsConfig::default();

        assert_eq!(queues_label(None), "all");
        assert_eq!(
            queues_label(Some(&["a".to_string(), "b".to_string()])),
            "a,b"
        );
        assert_eq!(
            cron_label(&config, true),
            format!("{}s", config.cron_interval)
        );
        assert_eq!(cron_label(&config, false), "off (retention purges only)");
    }
}
