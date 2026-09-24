//! Job execution dispatch: Lua handlers and the system email job, with the
//! hand-off to the image-convert and bulk system jobs.

use std::{sync::Arc, time::Instant};

use anyhow::{Context as _, Result, anyhow};
use serde_json::from_str;
use tracing::info;

use crate::{
    core::{
        JobDefinition, JobRun,
        email::{EmailJobData, EmailProvider, SYSTEM_EMAIL_JOB},
        job::SYSTEM_BULK_JOB,
        upload::{SYSTEM_IMAGE_CONVERT_JOB, SharedStorage},
    },
    db::{DbPool, query::jobs as job_query},
    hooks::{HookRunner, LuaCrudInfra},
    scheduler::{
        bulk::{ExecuteBulkParams, execute_system_bulk},
        runner::{
            failure::record_job_failure,
            image_convert::{ImageConvertRun, execute_system_image_convert},
        },
    },
    service::AppInfra,
};

/// Borrowed inputs for [`execute_job`], grouped per the >4-params rule.
/// All fields are references (or `Copy` options of references), so the
/// struct itself is `Copy` and passing it by value is free.
#[derive(Clone, Copy)]
pub struct ExecuteJobParams<'a> {
    pub pool: &'a DbPool,
    pub hook_runner: &'a HookRunner,
    pub job_def: &'a JobDefinition,
    pub job_run: &'a JobRun,
    pub email_provider: Option<&'a dyn EmailProvider>,
    pub storage: &'a SharedStorage,
    /// Event transport + populate cache for the handler's Lua CRUD calls
    /// (cloned per handler invocation; the queues stay `None` in pool-mode).
    /// `None` = job writes publish no events and skip cache invalidation.
    pub lua_infra: Option<&'a LuaCrudInfra>,
    /// Full infra bundle — required by `_system_bulk` (service-op
    /// execution). `None` in contexts that never run bulk jobs.
    pub app_infra: Option<&'a Arc<AppInfra>>,
}

/// Execute a single job: call the Lua handler with CRUD access,
/// or handle system jobs (`_system_email`, `_system_image_convert`)
/// directly in Rust.
///
/// # Errors
///
/// Returns an error if the connection acquisition, Lua hook execution,
/// system-job handler, or job-status update fails.
pub fn execute_job(p: ExecuteJobParams<'_>) -> Result<()> {
    let ExecuteJobParams {
        pool,
        hook_runner,
        job_def,
        job_run,
        email_provider,
        storage,
        lua_infra,
        app_infra,
    } = p;

    let start = Instant::now();

    info!(
        "Executing job {} ({}) attempt {}/{}",
        job_run.id, job_run.slug, job_run.attempt, job_run.max_attempts
    );

    // System email job: handle directly without Lua VM
    if job_run.slug == SYSTEM_EMAIL_JOB {
        return execute_system_email(pool, job_run, email_provider, start);
    }

    // System image-convert job: encode + write URL column + complete.
    // Rust handler — no Lua VM needed.
    if job_run.slug == SYSTEM_IMAGE_CONVERT_JOB {
        return execute_system_image_convert(
            &ImageConvertRun {
                pool,
                job_run,
                storage,
                app_infra: app_infra.map(AsRef::as_ref),
            },
            start,
        );
    }

    // System bulk job: run the queued bulk service op (create/update/delete
    // many) under the actor snapshotted at queue time. Rust handler.
    if job_run.slug == SYSTEM_BULK_JOB {
        return execute_system_bulk(&ExecuteBulkParams {
            pool,
            app_infra,
            job_run,
            start,
            timeout_secs: job_def.timeout,
        });
    }

    // Lua job handler runs in **pool-mode**: no outer transaction.
    // Each CRUD operation inside the handler opens its own short-lived
    // IMMEDIATE transaction (via `with_lua_db` / the `auto_tx` attribute
    // on every `#[lua_fn]` CRUD declaration). For multi-step atomicity
    // the user wraps a block in `crap.transaction(function() ... end)`,
    // which temporarily swaps the pool context for a shared tx context.
    // This avoids the `SQLITE_BUSY_SNAPSHOT` hazard that the previous
    // single-deferred-outer-tx model exposed for long-running handlers
    // that did read-then-write.
    let result = hook_runner.run_job_handler(job_def, job_run, pool, lua_infra.cloned());

    match result {
        Ok(result_json) => {
            // The write pool, like every job-row write: a write on a read
            // connection starves the readers the pool split protects.
            let c = pool
                .write()
                .context("Failed to get DB connection for completion")?;

            job_query::complete_job(&c, &job_run.id, job_run.attempt, result_json.as_deref())?;

            let elapsed = start.elapsed();

            info!(
                "Job {} ({}) completed in {:?}",
                job_run.id, job_run.slug, elapsed
            );
        }
        Err(e) => {
            record_job_failure(
                pool,
                job_run,
                &format!("Job {} ({})", job_run.id, job_run.slug),
                &e,
            )?;
        }
    }

    Ok(())
}

/// Execute a `_system_email` job: parse data and send via email provider.
fn execute_system_email(
    pool: &DbPool,
    job_run: &JobRun,
    email_provider: Option<&dyn EmailProvider>,
    start: Instant,
) -> Result<()> {
    let provider = email_provider
        .ok_or_else(|| anyhow!("System email job requires email provider but none configured"))?;

    let data: EmailJobData = from_str(&job_run.data).context("Invalid email job data")?;

    let result = provider.send(&data.to, &data.subject, &data.html, data.text.as_deref());

    match result {
        Ok(()) => {
            let c = pool
                .write()
                .context("Failed to get DB connection for email job completion")?;

            job_query::complete_job(&c, &job_run.id, job_run.attempt, None)?;

            let elapsed = start.elapsed();

            info!(
                "Email job {} completed in {:?} (to: {})",
                job_run.id, elapsed, data.to
            );
        }
        Err(e) => {
            record_job_failure(pool, job_run, &format!("Email job {}", job_run.id), &e)?;
        }
    }

    Ok(())
}
