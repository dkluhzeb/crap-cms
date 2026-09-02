//! `jobs` command — manage background jobs.

use std::path::Path;

use anyhow::{Context as _, Result, anyhow};
use serde_json::Value;

use crate::{
    cli::{self, Table},
    commands::{JobsAction, helpers::init_stack},
    config::{CrapConfig, JobsConfig, parse_duration_string},
    core::{
        Registry,
        job::{JobRun, JobStatus},
    },
    db::{DbPool, pool, query},
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
fn run_list(registry: &Registry, pool: &DbPool) -> Result<()> {
    let conn = pool.get().context("Failed to get DB connection")?;

    if registry.jobs.is_empty() {
        cli::info("No jobs defined.");

        return Ok(());
    }

    let mut table = Table::new(vec!["Job", "Schedule", "Queue", "Recent Runs"]);

    let mut slugs: Vec<_> = registry.jobs.keys().collect();
    slugs.sort();

    for slug in slugs {
        let def = &registry.jobs[slug];
        let schedule = def.schedule.as_deref().unwrap_or("-").to_string();
        let recent = query::jobs::list_job_runs(&conn, Some(slug), None, 5, 0).unwrap_or_default();
        let status_summary = summarize_recent_runs(&recent);

        table.row(vec![slug, &schedule, &def.queue, &status_summary]);
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

/// Overall job-system health, in ascending severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JobHealth {
    Healthy,
    Warning,
    Unhealthy,
}

impl JobHealth {
    /// Human label printed in the `Status` line.
    fn label(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Warning => "warning",
            Self::Unhealthy => "unhealthy",
        }
    }

    /// Process exit code. Mirrors `status --check` (2 = warnings) and
    /// `update check` (1 = action needed) so CI can gate on the result:
    /// 0 healthy, 2 warning, 1 unhealthy.
    fn exit_code(self) -> i32 {
        match self {
            Self::Healthy => 0,
            Self::Warning => 2,
            Self::Unhealthy => 1,
        }
    }
}

/// Classify the health probes: any stale (heartbeat-expired) running job
/// is `Unhealthy`; recent failures, long-pending jobs, or scheduled jobs
/// that never completed a run are `Warning`.
fn classify_health(
    stale: usize,
    failed_24h: i64,
    pending_long: i64,
    never_ran: usize,
) -> JobHealth {
    if stale > 0 {
        return JobHealth::Unhealthy;
    }

    if failed_24h > 0 || pending_long > 0 || never_ran > 0 {
        return JobHealth::Warning;
    }

    JobHealth::Healthy
}

fn run_healthcheck(cfg: &CrapConfig, registry: &Registry, pool: &DbPool) -> Result<JobHealth> {
    let conn = pool.get().context("Failed to get DB connection")?;

    let defined_count = registry.jobs.len();

    // Stale jobs: running but heartbeat expired (heartbeat_interval * 3)
    let stale_threshold = cfg.jobs.heartbeat_interval * 3;
    let stale_jobs = query::jobs::find_stale_jobs(&conn, stale_threshold)?;
    let stale_count = stale_jobs.len();

    // Failed jobs in the last 24 hours
    let failed_24h = query::jobs::count_failed_since(&conn, 86400)?;

    // Pending jobs waiting longer than 5 minutes
    let pending_long = query::jobs::count_pending_older_than(&conn, 300)?;

    // Check for scheduled jobs with no recent runs
    let mut no_recent_runs = Vec::new();
    for (slug, def) in &registry.jobs {
        if def.schedule.is_some() {
            let last = query::jobs::last_completed_run(&conn, slug)?;

            if last.is_none() {
                no_recent_runs.push(slug.to_string());
            }
        }
    }

    let health = classify_health(stale_count, failed_24h, pending_long, no_recent_runs.len());

    cli::header("Job system health");
    cli::kv("Defined", &defined_count.to_string());
    cli::kv("Stale", &stale_count.to_string());
    cli::kv("Failed 24h", &failed_24h.to_string());
    cli::kv("Pending 5m", &pending_long.to_string());

    if !no_recent_runs.is_empty() {
        no_recent_runs.sort();
        cli::kv("No runs", &no_recent_runs.join(", "));
    }
    cli::kv_status("Status", health.label(), health == JobHealth::Healthy);

    if stale_count > 0 {
        cli::header("Stale jobs");

        for job in &stale_jobs {
            cli::warning(&format!(
                "{} ({}): started {}, last heartbeat {}",
                job.id,
                job.slug,
                job.started_at.as_deref().unwrap_or("-"),
                job.heartbeat_at.as_deref().unwrap_or("never")
            ));
        }
    }

    Ok(health)
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

    let conn = pool.get().context("Failed to get DB connection")?;
    let job_run = query::jobs::insert_job(
        &conn,
        slug,
        data_json,
        "cli",
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
fn run_cancel(config_dir: &Path, slug: Option<String>) -> Result<()> {
    let config_dir = config_dir
        .canonicalize()
        .unwrap_or_else(|_| config_dir.to_path_buf());

    let cfg = CrapConfig::load(&config_dir)?;
    let pool = pool::create_pool(&config_dir, &cfg)?;
    let conn = pool.get().context("Failed to get DB connection")?;
    let deleted = query::jobs::cancel_pending_jobs(&conn, slug.as_deref())?;

    match slug {
        Some(s) => cli::success(&format!("Cancelled {deleted} pending '{s}' job(s)")),
        None => cli::success(&format!("Cancelled {deleted} pending job(s)")),
    }

    Ok(())
}

/// Purge old completed/failed job runs older than the specified duration.
#[cfg(not(tarpaulin_include))]
fn run_purge(config_dir: &Path, older_than: &str) -> Result<()> {
    let config_dir = config_dir
        .canonicalize()
        .unwrap_or_else(|_| config_dir.to_path_buf());

    let cfg = CrapConfig::load(&config_dir)?;
    let pool = pool::create_pool(&config_dir, &cfg)?;

    let secs = parse_duration_string(older_than).ok_or_else(|| {
        anyhow!(
            "Invalid duration '{older_than}'. Use format like '7d' (days), '24h' (hours), '30m' (minutes), '60s' (seconds)"
        )
    })?;

    let conn = pool.get().context("Failed to get DB connection")?;
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
            let (_cfg, registry, pool) = init_stack(config_dir)?;
            run_list(&registry, &pool)
        }
        JobsAction::Trigger {
            slug,
            data,
            priority,
        } => {
            let (cfg, registry, pool) = init_stack(config_dir)?;
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
            let (_cfg, _registry, pool) = init_stack(config_dir)?;
            run_status(&pool, id.as_deref(), slug.as_deref(), limit)
        }
        JobsAction::Cancel { slug } => run_cancel(config_dir, slug),
        JobsAction::Purge { older_than } => run_purge(config_dir, &older_than),
        JobsAction::Healthcheck => {
            let (cfg, registry, pool) = init_stack(config_dir)?;
            let health = run_healthcheck(&cfg, &registry, &pool)?;

            // CI usability: a non-healthy result must be distinguishable
            // from a healthy one by exit code (see `JobHealth::exit_code`).
            if health != JobHealth::Healthy {
                std::process::exit(health.exit_code());
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
    /// result, so a CI gate on it never fired.
    #[test]
    fn healthcheck_classification_and_exit_codes() {
        assert_eq!(classify_health(0, 0, 0, 0), JobHealth::Healthy);
        assert_eq!(classify_health(0, 1, 0, 0), JobHealth::Warning);
        assert_eq!(classify_health(0, 0, 3, 0), JobHealth::Warning);
        assert_eq!(classify_health(0, 0, 0, 2), JobHealth::Warning);
        // Stale wins over every warning signal.
        assert_eq!(classify_health(1, 5, 5, 5), JobHealth::Unhealthy);

        assert_eq!(JobHealth::Healthy.exit_code(), 0);
        assert_eq!(JobHealth::Warning.exit_code(), 2);
        assert_eq!(JobHealth::Unhealthy.exit_code(), 1);
        assert_eq!(JobHealth::Unhealthy.label(), "unhealthy");
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
}
