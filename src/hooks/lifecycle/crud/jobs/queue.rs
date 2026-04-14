//! Registration of `crap.jobs.queue` Lua function.

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Table, Value};

use crate::{
    core::SharedRegistry,
    db::{AccessResult, query},
    hooks::{
        api,
        lifecycle::{
            access::check_access_with_lua,
            crud::{get_tx_conn, helpers::hook_user},
        },
    },
};

/// Core logic for `crap.jobs.queue`.
fn queue_job_inner(
    lua: &Lua,
    reg: &SharedRegistry,
    slug: String,
    data: Option<Table>,
) -> mlua::Result<String> {
    // SAFETY: pointer valid for hook call duration — see TxContext pattern
    let conn_ptr = get_tx_conn(lua)?;
    let conn = unsafe { &*conn_ptr };

    let job_def = {
        let r = reg
            .read()
            .map_err(|e| RuntimeError(format!("Registry lock: {e:#}")))?;
        r.get_job(&slug)
            .cloned()
            .ok_or_else(|| RuntimeError(format!("Job '{}' not defined", slug)))?
    };

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
            let json_val = api::lua_to_json(lua, &Value::Table(tbl))?;

            serde_json::to_string(&json_val)
                .map_err(|e| RuntimeError(format!("JSON error: {e:#}")))?
        }
        None => "{}".to_string(),
    };

    let job_run = query::jobs::insert_job(
        conn,
        &slug,
        &data_json,
        "hook",
        job_def.retries + 1,
        &job_def.queue,
    )
    .map_err(|e| RuntimeError(format!("queue error: {e:#}")))?;

    Ok(job_run.id)
}

/// Register `crap.jobs.queue(slug, data?)`.
#[cfg(not(tarpaulin_include))]
pub(crate) fn register_jobs_queue(
    lua: &Lua,
    table: &Table,
    registry: SharedRegistry,
) -> Result<()> {
    let queue_fn = lua.create_function(move |lua, (slug, data): (String, Option<Table>)| {
        queue_job_inner(lua, &registry, slug, data)
    })?;

    table.set("queue", queue_fn)?;

    Ok(())
}
