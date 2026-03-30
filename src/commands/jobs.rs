//! `jobs` command — manage background jobs.

use anyhow::{Context as _, Result, anyhow};
use serde_json::Value;
use std::path::Path;

use super::JobsAction;
use crate::{
    cli::{self, Table},
    config::{CrapConfig, parse_duration_string},
    core::{SharedRegistry, job::JobStatus},
    db::{DbPool, migrate, pool, query},
    hooks,
};

/// Initialize config, Lua, pool, and migrate. Used by most job subcommands.
fn init_stack(config_dir: &Path) -> Result<(CrapConfig, SharedRegistry, DbPool)> {
    let config_dir = config_dir
        .canonicalize()
        .unwrap_or_else(|_| config_dir.to_path_buf());
    let cfg = CrapConfig::load(&config_dir)?;
    let registry = hooks::init_lua(&config_dir, &cfg)?;
    let pool = pool::create_pool(&config_dir, &cfg)?;
    migrate::sync_all(&pool, &registry, &cfg.locale)?;

    Ok((cfg, registry, pool))
}

/// List all defined jobs with recent run status summary.
fn run_list(registry: &SharedRegistry, pool: &DbPool) -> Result<()> {
    let reg = registry
        .read()
        .map_err(|e| anyhow!("Registry lock poisoned: {}", e))?;
    let conn = pool.get().context("Failed to get DB connection")?;

    if reg.jobs.is_empty() {
        cli::info("No jobs defined.");

        return Ok(());
    }

    let mut table = Table::new(vec!["Job", "Schedule", "Queue", "Recent Runs"]);

    let mut slugs: Vec<_> = reg.jobs.keys().collect();
    slugs.sort();

    for slug in slugs {
        let def = &reg.jobs[slug];
        let schedule = def.schedule.as_deref().unwrap_or("-").to_string();
        let recent = query::jobs::list_job_runs(&conn, Some(slug), None, 5, 0).unwrap_or_default();

        let status_summary = if recent.is_empty() {
            "none".to_string()
        } else {
            let completed = recent
                .iter()
                .filter(|r| r.status == JobStatus::Completed)
                .count();
            let failed = recent
                .iter()
                .filter(|r| r.status == JobStatus::Failed)
                .count();
            let pending = recent
                .iter()
                .filter(|r| r.status == JobStatus::Pending)
                .count();
            let running = recent
                .iter()
                .filter(|r| r.status == JobStatus::Running)
                .count();
            let mut parts = Vec::new();

            if completed > 0 {
                parts.push(format!("{}ok", completed));
            }
            if failed > 0 {
                parts.push(format!("{}fail", failed));
            }
            if pending > 0 {
                parts.push(format!("{}pend", pending));
            }
            if running > 0 {
                parts.push(format!("{}run", running));
            }
            parts.join("/")
        };

        table.row(vec![slug, &schedule, &def.queue, &status_summary]);
    }

    table.print();

    Ok(())
}

/// Show status for a single job run or list recent runs.
fn run_status(pool: &DbPool, id: Option<String>, slug: Option<String>, limit: i64) -> Result<()> {
    let conn = pool.get().context("Failed to get DB connection")?;

    if let Some(run_id) = id {
        let run = query::jobs::get_job_run(&conn, &run_id)?
            .ok_or_else(|| anyhow!("Job run '{}' not found", run_id))?;
        cli::kv("ID", &run.id);
        cli::kv("Job", &run.slug);
        cli::kv("Status", run.status.as_str());
        cli::kv("Queue", &run.queue);
        cli::kv("Attempt", &format!("{}/{}", run.attempt, run.max_attempts));
        cli::kv("Scheduled", run.scheduled_by.as_deref().unwrap_or("-"));
        cli::kv("Created", run.created_at.as_deref().unwrap_or("-"));
        cli::kv("Started", run.started_at.as_deref().unwrap_or("-"));
        cli::kv("Completed", run.completed_at.as_deref().unwrap_or("-"));

        if !run.data.is_empty() {
            cli::kv("Data", &run.data);
        }
        if let Some(ref result) = run.result {
            cli::kv("Result", &result.to_string());
        }
        if let Some(ref error) = run.error {
            cli::kv("Error", &error.to_string());
        }
    } else {
        let runs = query::jobs::list_job_runs(&conn, slug.as_deref(), None, limit, 0)?;

        if runs.is_empty() {
            cli::info("No job runs found.");

            return Ok(());
        }

        let mut table = Table::new(vec!["ID", "Job", "Status", "Attempt", "Error", "Created"]);

        for run in &runs {
            let attempt = format!("{}/{}", run.attempt, run.max_attempts);
            let error = run
                .error
                .as_deref()
                .map(|e| {
                    let truncated: String = e.chars().take(50).collect();
                    if truncated.len() < e.len() {
                        format!("{truncated}…")
                    } else {
                        truncated
                    }
                })
                .unwrap_or_default();

            table.row(vec![
                &run.id,
                &run.slug,
                run.status.as_str(),
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

/// Check job system health: stale, failed, pending, never-completed.
fn run_healthcheck(cfg: &CrapConfig, registry: &SharedRegistry, pool: &DbPool) -> Result<()> {
    let reg = registry
        .read()
        .map_err(|e| anyhow!("Registry lock poisoned: {}", e))?;
    let conn = pool.get().context("Failed to get DB connection")?;

    let defined_count = reg.jobs.len();

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
    for (slug, def) in &reg.jobs {
        if def.schedule.is_some() {
            let last = query::jobs::last_completed_run(&conn, slug)?;

            if last.is_none() {
                no_recent_runs.push(slug.to_string());
            }
        }
    }

    // Determine status
    let status = if stale_count > 0 {
        "unhealthy"
    } else if failed_24h > 0 || pending_long > 0 || !no_recent_runs.is_empty() {
        "warning"
    } else {
        "healthy"
    };

    cli::header("Job system health");
    cli::kv("Defined", &defined_count.to_string());
    cli::kv("Stale", &stale_count.to_string());
    cli::kv("Failed 24h", &failed_24h.to_string());
    cli::kv("Pending 5m", &pending_long.to_string());

    if !no_recent_runs.is_empty() {
        no_recent_runs.sort();
        cli::kv("No runs", &no_recent_runs.join(", "));
    }
    cli::kv_status("Status", status, status == "healthy");

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

    Ok(())
}

/// Handle the `jobs` subcommand.
// Excluded from coverage: requires full Lua + DB setup (init_lua, create_pool, sync_all)
// for each subcommand variant. Tested via CLI integration tests.
#[cfg(not(tarpaulin_include))]
pub fn run(config_dir: &Path, action: JobsAction) -> Result<()> {
    match action {
        JobsAction::List => {
            let (_cfg, registry, pool) = init_stack(config_dir)?;
            run_list(&registry, &pool)
        }
        JobsAction::Trigger { slug, data } => {
            let (_cfg, registry, pool) = init_stack(config_dir)?;
            let reg = registry
                .read()
                .map_err(|e| anyhow!("Registry lock poisoned: {}", e))?;

            let job_def = reg
                .get_job(&slug)
                .ok_or_else(|| anyhow!("Job '{}' not defined", slug))?;

            let data_json = data.as_deref().unwrap_or("{}");
            serde_json::from_str::<Value>(data_json).context("Invalid JSON data")?;

            let conn = pool.get().context("Failed to get DB connection")?;
            let job_run = query::jobs::insert_job(
                &conn,
                &slug,
                data_json,
                "cli",
                job_def.retries + 1,
                &job_def.queue,
            )?;

            cli::success(&format!("Queued job '{}' (run {})", slug, job_run.id));
            cli::hint("The job will be picked up by the scheduler when the server is running.");

            Ok(())
        }
        JobsAction::Status { id, slug, limit } => {
            let (_cfg, _registry, pool) = init_stack(config_dir)?;
            run_status(&pool, id, slug, limit)
        }
        JobsAction::Cancel { slug } => {
            let config_dir = config_dir
                .canonicalize()
                .unwrap_or_else(|_| config_dir.to_path_buf());
            let cfg = CrapConfig::load(&config_dir)?;
            let pool = pool::create_pool(&config_dir, &cfg)?;
            let conn = pool.get().context("Failed to get DB connection")?;
            let deleted = query::jobs::cancel_pending_jobs(&conn, slug.as_deref())?;
            match slug {
                Some(s) => cli::success(&format!("Cancelled {} pending '{}' job(s)", deleted, s)),
                None => cli::success(&format!("Cancelled {} pending job(s)", deleted)),
            }
            Ok(())
        }
        JobsAction::Purge { older_than } => {
            let config_dir = config_dir
                .canonicalize()
                .unwrap_or_else(|_| config_dir.to_path_buf());
            let cfg = CrapConfig::load(&config_dir)?;
            let pool = pool::create_pool(&config_dir, &cfg)?;

            let secs = parse_duration_string(&older_than)
                .ok_or_else(|| anyhow!(
                    "Invalid duration '{}'. Use format like '7d' (days), '24h' (hours), '30m' (minutes), '60s' (seconds)",
                    older_than
                ))?;

            let conn = pool.get().context("Failed to get DB connection")?;
            let deleted = query::jobs::purge_old_jobs(&conn, secs)?;
            cli::success(&format!("Purged {} old job run(s)", deleted));

            Ok(())
        }
        JobsAction::Healthcheck => {
            let (cfg, registry, pool) = init_stack(config_dir)?;
            run_healthcheck(&cfg, &registry, &pool)
        }
    }
}
