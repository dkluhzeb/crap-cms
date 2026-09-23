//! `jobs` command — manage background jobs.

use std::path::Path;

use anyhow::{Context as _, Result, anyhow};
use serde_json::Value;

use crate::{
    cli::{self, Table},
    commands::{
        JobsAction,
        helpers::{Project, open_project},
    },
    config::{CrapConfig, JobsConfig, parse_duration_string},
    core::{
        Registry, ScheduledBy,
        job::{JobRun, JobStatus, is_system_job_slug},
    },
    db::{DbPool, query},
    service::{
        self,
        jobs::{JobHealthReport, JobHealthStatus},
    },
};

/// Summarize a batch of recent job runs as a compact `Nok/Mfail/Ppend/Qrun`
/// string, or `"none"` when the batch is empty. Only non-zero buckets appear,
/// always in completed→failed→pending→running order.
fn summarize_recent_runs(runs: &[JobRun]) -> String {
    if runs.is_empty() {
        return "none".to_string();
    }

    let count = |status: JobStatus| runs.iter().filter(|r| r.status == status).count();

    let buckets = [
        (count(JobStatus::Completed), "ok"),
        (count(JobStatus::Failed), "fail"),
        (count(JobStatus::Pending), "pend"),
        (count(JobStatus::Running), "run"),
    ];

    buckets
        .iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, label)| format!("{n}{label}"))
        .collect::<Vec<_>>()
        .join("/")
}

/// Truncate `s` to at most `max_chars` characters, appending `…` when the
/// string was actually shortened. Counts characters (not bytes), so it never
/// splits a multi-byte char.
fn truncate_with_ellipsis(s: &str, max_chars: usize) -> String {
    let truncated: String = s.chars().take(max_chars).collect();

    if truncated.len() < s.len() {
        format!("{truncated}…")
    } else {
        truncated
    }
}

/// List all defined jobs with recent run status summary.
///
/// The operator view: no access gate, because the CLI has no user to gate on.
fn run_list(registry: &Registry, pool: &DbPool, jobs_config: &JobsConfig) -> Result<()> {
    let conn = pool.get().context("Failed to get DB connection")?;

    let jobs = service::jobs::job_definitions(registry, &jobs_config.queue_retries());

    if jobs.is_empty() {
        cli::info("No jobs defined.");

        return Ok(());
    }

    let mut table = Table::new(vec!["Job", "Schedule", "Queue", "Retries", "Recent Runs"]);

    for job in &jobs {
        let schedule = job.schedule.as_deref().unwrap_or("-").to_string();
        let retries = job.retries.to_string();
        let recent =
            query::jobs::list_job_runs(&conn, Some(&job.slug), None, 5, 0).unwrap_or_default();
        let status_summary = summarize_recent_runs(&recent);

        table.row(vec![
            &job.slug,
            &schedule,
            &job.queue,
            &retries,
            &status_summary,
        ]);
    }

    table.print();

    Ok(())
}

/// Show status for a single job run or list recent runs.
fn run_status(pool: &DbPool, id: Option<&str>, slug: Option<&str>, limit: i64) -> Result<()> {
    let conn = pool.get().context("Failed to get DB connection")?;

    if let Some(run_id) = id {
        let run = query::jobs::get_job_run(&conn, run_id)?
            .ok_or_else(|| anyhow!("Job run '{run_id}' not found"))?;

        cli::kv("ID", &run.id);
        cli::kv("Job", &run.slug);
        cli::kv("Status", run.status.as_str());
        cli::kv("Queue", &run.queue);
        cli::kv("Priority", &run.priority.to_string());
        cli::kv("Attempt", &format!("{}/{}", run.attempt, run.max_attempts));
        cli::kv("Scheduled", run.scheduled_by.as_deref().unwrap_or("-"));
        cli::kv("Created", run.created_at.as_deref().unwrap_or("-"));
        cli::kv("Started", run.started_at.as_deref().unwrap_or("-"));
        cli::kv("Completed", run.completed_at.as_deref().unwrap_or("-"));

        if !run.data.is_empty() {
            cli::kv("Data", &run.data);
        }

        if let Some(ref result) = run.result {
            cli::kv("Result", &result.clone());
        }

        if let Some(ref error) = run.error {
            cli::kv("Error", &error.clone());
        }
    } else {
        let runs = query::jobs::list_job_runs(&conn, slug, None, limit, 0)?;

        if runs.is_empty() {
            cli::info("No job runs found.");

            return Ok(());
        }

        let mut table = Table::new(vec![
            "ID", "Job", "Status", "Prio", "Attempt", "Error", "Created",
        ]);

        for run in &runs {
            let attempt = format!("{}/{}", run.attempt, run.max_attempts);
            let priority = run.priority.to_string();
            let error = run
                .error
                .as_deref()
                .map(|e| truncate_with_ellipsis(e, 50))
                .unwrap_or_default();

            table.row(vec![
                &run.id,
                &run.slug,
                run.status.as_str(),
                &priority,
                &attempt,
                &error,
                run.created_at.as_deref().unwrap_or("-"),
            ]);
        }

        table.print();
        table.footer(&format!("{} run(s)", runs.len()));
    }

    Ok(())
}

/// Process exit code for a health verdict. Mirrors `status --check`
/// (2 = warnings) and `update check` (1 = action needed) so CI can gate on
/// the result: 0 healthy, 2 warning, 1 unhealthy.
fn health_exit_code(status: JobHealthStatus) -> i32 {
    match status {
        JobHealthStatus::Healthy => 0,
        JobHealthStatus::Warning => 2,
        JobHealthStatus::Unhealthy => 1,
    }
}

/// Print the health report; the stale runs get named so an operator can go
/// look at them.
fn print_health(report: &JobHealthReport) {
    cli::header("Job system health");
    cli::kv("Defined", &report.defined.to_string());
    cli::kv("Stale", &report.stale.len().to_string());
    cli::kv("Failed 24h", &report.failed_recently.to_string());
    cli::kv("Pending 5m", &report.pending_long.to_string());

    if !report.never_ran.is_empty() {
        cli::kv("No runs", &report.never_ran.join(", "));
    }

    cli::kv_status(
        "Status",
        report.status.as_str(),
        report.status == JobHealthStatus::Healthy,
    );

    if report.stale.is_empty() {
        return;
    }

    cli::header("Stale jobs");

    for job in &report.stale {
        cli::warning(&format!(
            "{} ({}): started {}, last heartbeat {}",
            job.id,
            job.slug,
            job.started_at.as_deref().unwrap_or("-"),
            job.heartbeat_at.as_deref().unwrap_or("never")
        ));
    }
}

/// Probe the job system and report. The verdict — and the line at which a
/// running job counts as dead — come from the service layer, so this agrees
/// with what the scheduler actually reclaims.
fn run_healthcheck(
    cfg: &CrapConfig,
    registry: &Registry,
    pool: &DbPool,
) -> Result<JobHealthStatus> {
    let conn = pool.get().context("Failed to get DB connection")?;

    let report = service::jobs::check_job_health(&conn, registry, cfg)?;
    print_health(&report);

    Ok(report.status)
}

/// Trigger a job manually by slug, queuing it for the scheduler.
#[cfg(not(tarpaulin_include))]
fn run_trigger(
    registry: &Registry,
    pool: &DbPool,
    jobs_config: &JobsConfig,
    slug: &str,
    data: Option<&str>,
    priority: Option<i32>,
) -> Result<()> {
    // System jobs are queued only by the subsystem that owns each one — never
    // by slug, from any surface. Refused before the registry lookup so the
    // answer matches an undefined job.
    if is_system_job_slug(slug) {
        return Err(anyhow!("Job '{slug}' not defined"));
    }

    let job_def = registry
        .get_job(slug)
        .ok_or_else(|| anyhow!("Job '{slug}' not defined"))?;

    let data_json = data.unwrap_or("{}");

    serde_json::from_str::<Value>(data_json).context("Invalid JSON data")?;

    let effective_priority = priority.unwrap_or(job_def.priority);
    let queue_retries = jobs_config
        .queues
        .get(&job_def.queue)
        .and_then(|q| q.retries);

    let conn = pool.write().context("Failed to get a write connection")?;
    let job_run = query::jobs::insert_job(
        &conn,
        slug,
        data_json,
        ScheduledBy::Cli,
        job_def.effective_max_attempts(queue_retries),
        &job_def.queue,
        effective_priority,
    )?;

    cli::success(&format!("Queued job '{}' (run {})", slug, job_run.id));
    cli::hint("The job will be picked up by the scheduler when the server is running.");

    Ok(())
}

/// Cancel pending jobs, optionally filtered by slug.
#[cfg(not(tarpaulin_include))]
fn run_cancel(pool: &DbPool, slug: Option<String>, id: Option<String>) -> Result<()> {
    let conn = pool.write().context("Failed to get a write connection")?;

    // A single run by id — the precise alternative to clearing a whole
    // slug, which would discard every other caller's pending work.
    if let Some(id) = id {
        if query::jobs::cancel_pending_job(&conn, &id)? {
            cli::success(&format!("Cancelled pending job run {id}"));
        } else {
            cli::warning(&format!(
                "No pending job run {id} (it may have already been claimed)"
            ));
        }

        return Ok(());
    }

    let deleted = query::jobs::cancel_pending_jobs(&conn, slug.as_deref())?;

    match slug {
        Some(s) => cli::success(&format!("Cancelled {deleted} pending '{s}' job(s)")),
        None => cli::success(&format!("Cancelled {deleted} pending job(s)")),
    }

    Ok(())
}

/// Parse the `--older-than` duration of `jobs purge` into seconds.
fn parse_purge_age(older_than: &str) -> Result<u64> {
    parse_duration_string(older_than).ok_or_else(|| {
        anyhow!(
            "Invalid duration '{older_than}'. Use format like '7d' (days), '24h' (hours), '30m' (minutes), '60s' (seconds)"
        )
    })
}

/// Purge old completed/failed job runs older than the specified duration.
#[cfg(not(tarpaulin_include))]
fn run_purge(pool: &DbPool, secs: u64) -> Result<()> {
    let conn = pool.write().context("Failed to get a write connection")?;
    let deleted = query::jobs::purge_old_jobs(&conn, secs)?;

    cli::success(&format!("Purged {deleted} old job run(s)"));

    Ok(())
}

/// Handle the `jobs` subcommand — dispatches to the appropriate action handler.
///
/// # Errors
///
/// Returns an error if config loading, pool creation, or the dispatched
/// action fails.
#[cfg(not(tarpaulin_include))]
pub fn run(config_dir: &Path, action: JobsAction) -> Result<()> {
    match action {
        JobsAction::List => {
            let Project {
                lock: _instance_lock,
                config: cfg,
                registry,
                pool,
            } = open_project(config_dir)?;
            run_list(&registry, &pool, &cfg.jobs)
        }
        JobsAction::Trigger {
            slug,
            data,
            priority,
        } => {
            let Project {
                lock: _instance_lock,
                config: cfg,
                registry,
                pool,
            } = open_project(config_dir)?;
            run_trigger(
                &registry,
                &pool,
                &cfg.jobs,
                &slug,
                data.as_deref(),
                priority,
            )
        }
        JobsAction::Status { id, slug, limit } => {
            let Project {
                lock: _instance_lock,
                config: _cfg,
                registry: _registry,
                pool,
            } = open_project(config_dir)?;
            run_status(&pool, id.as_deref(), slug.as_deref(), limit)
        }
        JobsAction::Cancel { slug, id } => {
            let Project {
                lock: _instance_lock,
                config: _cfg,
                registry: _registry,
                pool,
            } = open_project(config_dir)?;
            run_cancel(&pool, slug, id)
        }
        JobsAction::Purge { older_than } => {
            let secs = parse_purge_age(&older_than)?;
            let Project {
                lock: _instance_lock,
                config: _cfg,
                registry: _registry,
                pool,
            } = open_project(config_dir)?;
            run_purge(&pool, secs)
        }
        JobsAction::Healthcheck => {
            let Project {
                lock: _instance_lock,
                config: cfg,
                registry,
                pool,
            } = open_project(config_dir)?;
            let health = run_healthcheck(&cfg, &registry, &pool)?;

            // CI usability: a non-healthy result must be distinguishable
            // from a healthy one by exit code.
            if health != JobHealthStatus::Healthy {
                std::process::exit(health_exit_code(health));
            }

            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(status: JobStatus) -> JobRun {
        JobRun::builder("r", "cleanup").status(status).build()
    }

    /// Regression: `jobs healthcheck` used to exit 0 regardless of the
    /// result, so a CI gate on it never fired. The classification itself is
    /// pinned next to the probes in the service layer.
    #[test]
    fn healthcheck_exit_codes_separate_the_verdicts() {
        assert_eq!(health_exit_code(JobHealthStatus::Healthy), 0);
        assert_eq!(health_exit_code(JobHealthStatus::Warning), 2);
        assert_eq!(health_exit_code(JobHealthStatus::Unhealthy), 1);
        assert_eq!(JobHealthStatus::Unhealthy.as_str(), "unhealthy");
    }

    #[test]
    fn empty_batch_summarizes_as_none() {
        assert_eq!(summarize_recent_runs(&[]), "none");
    }

    #[test]
    fn buckets_appear_in_fixed_order_with_counts() {
        let runs = [
            run(JobStatus::Completed),
            run(JobStatus::Completed),
            run(JobStatus::Failed),
            run(JobStatus::Running),
        ];
        // completed→failed→pending→running; pending bucket is omitted (zero).
        assert_eq!(summarize_recent_runs(&runs), "2ok/1fail/1run");
    }

    #[test]
    fn single_status_has_no_separator() {
        assert_eq!(summarize_recent_runs(&[run(JobStatus::Pending)]), "1pend");
    }

    #[test]
    fn truncate_leaves_short_strings_untouched() {
        assert_eq!(truncate_with_ellipsis("boom", 50), "boom");
        assert_eq!(truncate_with_ellipsis("", 50), "");
    }

    #[test]
    fn truncate_shortens_and_appends_ellipsis() {
        let long = "x".repeat(60);
        let out = truncate_with_ellipsis(&long, 50);
        assert_eq!(out.chars().filter(|&c| c == 'x').count(), 50);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn truncate_at_exact_length_adds_no_ellipsis() {
        let s = "x".repeat(50);
        assert_eq!(truncate_with_ellipsis(&s, 50), s);
    }

    #[test]
    fn truncate_counts_characters_not_bytes() {
        // 4 multi-byte chars, limit 2 → keep 2 chars + ellipsis (never splits).
        assert_eq!(truncate_with_ellipsis("héllo", 2), "hé…");
    }

    #[test]
    fn purge_age_parses_durations_and_names_the_bad_one() {
        assert_eq!(parse_purge_age("7d").unwrap(), 7 * 24 * 3600);

        let err = parse_purge_age("soon").unwrap_err().to_string();
        assert!(err.contains("Invalid duration 'soon'"), "{err}");
    }
}
