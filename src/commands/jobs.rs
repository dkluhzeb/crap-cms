//! `jobs` command — manage background jobs.

use anyhow::{Context as _, Result};

/// Handle the `jobs` subcommand.
// Excluded from coverage: requires full Lua + DB setup (init_lua, create_pool, sync_all)
// for each subcommand variant. Tested via CLI integration tests.
#[cfg(not(tarpaulin_include))]
pub fn run(action: super::JobsAction) -> Result<()> {
    match action {
        super::JobsAction::List { config } => {
            let config_dir = config.canonicalize().unwrap_or(config);
            let cfg = crate::config::CrapConfig::load(&config_dir)?;
            let registry = crate::hooks::init_lua(&config_dir, &cfg)?;
            let pool = crate::db::pool::create_pool(&config_dir, &cfg)?;
            crate::db::migrate::sync_all(&pool, &registry, &cfg.locale)?;

            let reg = registry.read()
                .map_err(|e| anyhow::anyhow!("Registry lock poisoned: {}", e))?;
            let conn = pool.get().context("Failed to get DB connection")?;

            if reg.jobs.is_empty() {
                println!("No jobs defined.");
                return Ok(());
            }

            println!("{:<24} {:<16} {:<16} {:<12}", "Job", "Schedule", "Queue", "Recent Runs");
            println!("{}", "-".repeat(70));

            let mut slugs: Vec<_> = reg.jobs.keys().collect();
            slugs.sort();

            for slug in slugs {
                let def = &reg.jobs[slug];
                let schedule = def.schedule.as_deref().unwrap_or("-");
                let recent = crate::db::query::jobs::list_job_runs(&conn, Some(slug), None, 5, 0)
                    .unwrap_or_default();

                let status_summary = if recent.is_empty() {
                    "none".to_string()
                } else {
                    let completed = recent.iter().filter(|r| r.status == crate::core::job::JobStatus::Completed).count();
                    let failed = recent.iter().filter(|r| r.status == crate::core::job::JobStatus::Failed).count();
                    let pending = recent.iter().filter(|r| r.status == crate::core::job::JobStatus::Pending).count();
                    let running = recent.iter().filter(|r| r.status == crate::core::job::JobStatus::Running).count();
                    let mut parts = Vec::new();
                    if completed > 0 { parts.push(format!("{}ok", completed)); }
                    if failed > 0 { parts.push(format!("{}fail", failed)); }
                    if pending > 0 { parts.push(format!("{}pend", pending)); }
                    if running > 0 { parts.push(format!("{}run", running)); }
                    parts.join("/")
                };

                println!("{:<24} {:<16} {:<16} {}", slug, schedule, &def.queue, status_summary);
            }

            Ok(())
        }
        super::JobsAction::Trigger { config, slug, data } => {
            let config_dir = config.canonicalize().unwrap_or(config);
            let cfg = crate::config::CrapConfig::load(&config_dir)?;
            let registry = crate::hooks::init_lua(&config_dir, &cfg)?;
            let pool = crate::db::pool::create_pool(&config_dir, &cfg)?;
            crate::db::migrate::sync_all(&pool, &registry, &cfg.locale)?;

            let reg = registry.read()
                .map_err(|e| anyhow::anyhow!("Registry lock poisoned: {}", e))?;

            let job_def = reg.get_job(&slug)
                .ok_or_else(|| anyhow::anyhow!("Job '{}' not defined", slug))?;

            let data_json = data.as_deref().unwrap_or("{}");

            // Validate JSON
            serde_json::from_str::<serde_json::Value>(data_json)
                .context("Invalid JSON data")?;

            let conn = pool.get().context("Failed to get DB connection")?;
            let job_run = crate::db::query::jobs::insert_job(
                &conn,
                &slug,
                data_json,
                "cli",
                job_def.retries + 1,
                &job_def.queue,
            )?;

            println!("Queued job '{}' (run {})", slug, job_run.id);
            println!("The job will be picked up by the scheduler when the server is running.");

            Ok(())
        }
        super::JobsAction::Status { config, id, slug, limit } => {
            let config_dir = config.canonicalize().unwrap_or(config);
            let cfg = crate::config::CrapConfig::load(&config_dir)?;
            let registry = crate::hooks::init_lua(&config_dir, &cfg)?;
            let pool = crate::db::pool::create_pool(&config_dir, &cfg)?;
            crate::db::migrate::sync_all(&pool, &registry, &cfg.locale)?;

            let conn = pool.get().context("Failed to get DB connection")?;

            if let Some(run_id) = id {
                // Show single run
                let run = crate::db::query::jobs::get_job_run(&conn, &run_id)?
                    .ok_or_else(|| anyhow::anyhow!("Job run '{}' not found", run_id))?;
                println!("ID:          {}", run.id);
                println!("Job:         {}", run.slug);
                println!("Status:      {}", run.status.as_str());
                println!("Queue:       {}", run.queue);
                println!("Attempt:     {}/{}", run.attempt, run.max_attempts);
                println!("Scheduled by: {}", run.scheduled_by.as_deref().unwrap_or("-"));
                println!("Created:     {}", run.created_at.as_deref().unwrap_or("-"));
                println!("Started:     {}", run.started_at.as_deref().unwrap_or("-"));
                println!("Completed:   {}", run.completed_at.as_deref().unwrap_or("-"));
                if let Some(ref data) = Some(&run.data) {
                    println!("Data:        {}", data);
                }
                if let Some(ref result) = run.result {
                    println!("Result:      {}", result);
                }
                if let Some(ref error) = run.error {
                    println!("Error:       {}", error);
                }
            } else {
                // List runs
                let runs = crate::db::query::jobs::list_job_runs(&conn, slug.as_deref(), None, limit, 0)?;
                if runs.is_empty() {
                    println!("No job runs found.");
                    return Ok(());
                }

                println!("{:<24} {:<20} {:<12} {:<8} {:<20}", "ID", "Job", "Status", "Attempt", "Created");
                println!("{}", "-".repeat(86));

                for run in &runs {
                    println!("{:<24} {:<20} {:<12} {}/{:<5} {}",
                        run.id, run.slug, run.status.as_str(),
                        run.attempt, run.max_attempts,
                        run.created_at.as_deref().unwrap_or("-"));
                }

                println!("\n{} run(s)", runs.len());
            }

            Ok(())
        }
        super::JobsAction::Purge { config, older_than } => {
            let config_dir = config.canonicalize().unwrap_or(config);
            let cfg = crate::config::CrapConfig::load(&config_dir)?;
            let pool = crate::db::pool::create_pool(&config_dir, &cfg)?;

            // Parse duration string
            let secs = crate::config::parse_duration_string(&older_than)
                .ok_or_else(|| anyhow::anyhow!(
                    "Invalid duration '{}'. Use format like '7d' (days), '24h' (hours), '30m' (minutes), '60s' (seconds)",
                    older_than
                ))?;

            let conn = pool.get().context("Failed to get DB connection")?;
            let deleted = crate::db::query::jobs::purge_old_jobs(&conn, secs)?;
            println!("Purged {} old job run(s)", deleted);

            Ok(())
        }
        super::JobsAction::Healthcheck { config } => {
            let config_dir = config.canonicalize().unwrap_or(config);
            let cfg = crate::config::CrapConfig::load(&config_dir)?;
            let registry = crate::hooks::init_lua(&config_dir, &cfg)?;
            let pool = crate::db::pool::create_pool(&config_dir, &cfg)?;
            crate::db::migrate::sync_all(&pool, &registry, &cfg.locale)?;

            let reg = registry.read()
                .map_err(|e| anyhow::anyhow!("Registry lock poisoned: {}", e))?;
            let conn = pool.get().context("Failed to get DB connection")?;

            let defined_count = reg.jobs.len();

            // Stale jobs: running but heartbeat expired (heartbeat_interval * 3)
            let stale_threshold = cfg.jobs.heartbeat_interval * 3;
            let stale_jobs = crate::db::query::jobs::find_stale_jobs(&conn, stale_threshold)?;
            let stale_count = stale_jobs.len();

            // Failed jobs in the last 24 hours
            let failed_24h = crate::db::query::jobs::count_failed_since(&conn, 86400)?;

            // Pending jobs waiting longer than 5 minutes
            let pending_long = crate::db::query::jobs::count_pending_older_than(&conn, 300)?;

            // Check for scheduled jobs with no recent runs
            let mut no_recent_runs = Vec::new();
            for (slug, def) in &reg.jobs {
                if def.schedule.is_some() {
                    let last = crate::db::query::jobs::last_completed_run(&conn, slug)?;
                    if last.is_none() {
                        // Scheduled job has never completed
                        no_recent_runs.push(slug.clone());
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

            println!("Job system health:");
            println!("  Defined jobs:    {}", defined_count);
            println!("  Stale jobs:      {}", stale_count);
            println!("  Failed (24h):    {}", failed_24h);
            println!("  Pending > 5min:  {}", pending_long);
            if !no_recent_runs.is_empty() {
                no_recent_runs.sort();
                println!("  Never completed: {}", no_recent_runs.join(", "));
            }
            println!("  Status: {}", status);

            if stale_count > 0 {
                println!("\nStale jobs:");
                for job in &stale_jobs {
                    println!("  {} ({}): started {}, last heartbeat {}",
                        job.id, job.slug,
                        job.started_at.as_deref().unwrap_or("-"),
                        job.heartbeat_at.as_deref().unwrap_or("never"));
                }
            }

            Ok(())
        }
    }
}

// parse_duration_string moved to crate::config — tests are there
