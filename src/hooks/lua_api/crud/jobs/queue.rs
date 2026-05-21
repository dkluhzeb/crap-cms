//! Registration of `crap.jobs.queue` Lua function.

use std::sync::Arc;

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Result as LuaResult, Table, Value};

use crate::core::Registry;
use crate::db::{AccessResult, query};
use crate::hooks::lifecycle::access::check_access_with_lua;
use crate::hooks::lua_api;
use crate::hooks::lua_api::crud::{get_tx_conn, helpers::hook_user};
use crate::typegen::lua::{LuaFnSpec, LuaParam, LuaReturn, lua_fn, lua_table};

/// Queue a job for background execution. Returns the job run ID.
/// Only available inside hooks with transaction context.
#[lua_fn(path = "crap.jobs.queue", returns_doc = "The queued job run ID.")]
fn jobs_queue(
    state: &Arc<Registry>,
    lua: &Lua,
    #[lua(doc = "Job slug (must be previously defined).")] slug: String,
    #[lua(doc = "Input data passed to the handler (default: {}).")] data: Option<Table>,
) -> LuaResult<String> {
    queue_job_inner(lua, state, &slug, data)
}

lua_table! {
    name: crap_jobs_queue,
    path: "crap.jobs",
    state: Arc<Registry>,
    fns: [jobs_queue],
}

/// Register `crap.jobs.queue(slug, data?)`. Parent `crap.jobs` table
/// must already exist (populated by `register_jobs_init` or
/// `register_jobs_pool_init`).
#[cfg(not(tarpaulin_include))]
pub(crate) fn register_jobs_queue(
    lua: &Lua,
    _table: &Table,
    registry: Arc<Registry>,
) -> Result<()> {
    register_crap_jobs_queue(lua, registry)?;
    Ok(())
}

/// Core logic for `crap.jobs.queue`. Extracted so the `#[lua_fn]` wrapper
/// stays thin and the hot path is easy to read on its own.
fn queue_job_inner(
    lua: &Lua,
    reg: &Registry,
    slug: &str,
    data: Option<Table>,
) -> LuaResult<String> {
    let conn = get_tx_conn(lua)?;

    let job_def = reg
        .get_job(slug)
        .cloned()
        .ok_or_else(|| RuntimeError(format!("Job '{slug}' not defined")))?;

    if job_def.access.is_some() {
        let user_doc = hook_user(lua);
        let result = check_access_with_lua(
            lua,
            job_def.access.as_deref(),
            user_doc.as_ref(),
            None,
            None,
        )
        .map_err(|e| RuntimeError(format!("access check error: {e:#}")))?;

        if matches!(result, AccessResult::Denied) {
            return Err(RuntimeError("Trigger access denied".to_string()));
        }

        if matches!(result, AccessResult::Constrained(_)) {
            return Err(RuntimeError(format!(
                "Access hook for job '{slug}' returned a filter table; job access is trigger-only — return true/false based on ctx.user fields instead."
            )));
        }
    }

    let data_json = match data {
        Some(tbl) => {
            let json_val = lua_api::lua_to_json(&Value::Table(tbl))?;

            serde_json::to_string(&json_val)
                .map_err(|e| RuntimeError(format!("JSON error: {e:#}")))?
        }
        None => "{}".to_string(),
    };

    let job_run = query::jobs::insert_job(
        conn,
        slug,
        &data_json,
        "hook",
        job_def.retries + 1,
        &job_def.queue,
    )
    .map_err(|e| RuntimeError(format!("queue error: {e:#}")))?;

    Ok(job_run.id)
}
