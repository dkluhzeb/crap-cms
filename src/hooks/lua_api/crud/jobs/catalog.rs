//! `crap.jobs.list` — which jobs exist.
//!
//! The run functions next door answer "what happened"; this answers "what can
//! happen". Without it a hook, a custom route or an admin page could read runs
//! but had no way to enumerate the jobs that produce them, and would have to
//! hardcode a list of slugs that drifts from the definitions.
//!
//! A thin wrapper over the same `service::jobs::list_jobs` chokepoint the gRPC
//! `ListJobs` RPC and the MCP `list_jobs` tool call, so the visibility gate and
//! the description of a job cannot drift between surfaces.

use std::{collections::HashMap, sync::Arc};

use mlua::{Error::RuntimeError, Lua, Result as LuaResult, Table};

use crate::{
    core::Registry,
    hooks::lua_api::crud::{helpers::hook_user, tx_conn::get_tx_conn},
    service::{self, LuaWriteHooks, ServiceContext, jobs::JobDefinitionInfo},
    typegen::lua::{LuaFnSpec, LuaReturn, lua_fn, lua_table},
};

/// Registry and queue defaults for the job catalog functions.
pub struct JobsCatalogState {
    pub registry: Arc<Registry>,
    /// The operator's per-queue `retries`, so a listed job reports the retry
    /// count it actually runs with.
    pub queue_retries: HashMap<String, u32>,
}

/// Convert a described job into the Lua table shape.
fn job_to_table(lua: &Lua, job: &JobDefinitionInfo) -> LuaResult<Table> {
    let t = lua.create_table()?;

    t.set("slug", job.slug.clone())?;
    t.set("queue", job.queue.clone())?;
    t.set("schedule", job.schedule.clone())?;
    t.set("timeout", job.timeout)?;
    t.set("priority", job.priority)?;
    t.set("retries", job.retries)?;
    t.set("concurrency", job.concurrency)?;
    t.set("skip_if_running", job.skip_if_running)?;
    t.set("label", job.label.clone())?;

    Ok(t)
}

/// List the defined jobs this caller may see.
#[lua_fn(
    path = "crap.jobs.list",
    returns = "crap.JobDefinitionInfo[]",
    returns_doc = "The defined jobs this caller may see, in slug order. A job whose access rule denies the caller is absent.",
    auto_tx_read
)]
fn jobs_list(state: &JobsCatalogState, lua: &Lua) -> LuaResult<Table> {
    let conn = get_tx_conn(lua)?;
    let user = hook_user(lua);
    let hooks = LuaWriteHooks::builder(lua).build();

    let ctx = ServiceContext::slug_only("")
        .conn(conn)
        .write_hooks(&hooks)
        .user(user.as_ref())
        .build();

    let jobs = service::jobs::list_jobs(&ctx, conn, &state.registry, &state.queue_retries)
        .map_err(|e| RuntimeError(format!("jobs.list: {e}")))?;

    let out = lua.create_table()?;

    for (i, job) in jobs.iter().enumerate() {
        out.raw_set(i + 1, job_to_table(lua, job)?)?;
    }

    Ok(out)
}

lua_table! {
    name: crap_jobs_catalog,
    path: "crap.jobs",
    state: JobsCatalogState,
    fns: [jobs_list],
}

/// Register the job catalog functions on `crap.jobs`.
#[cfg(not(tarpaulin_include))]
pub fn register_jobs_catalog(lua: &Lua, state: JobsCatalogState) -> anyhow::Result<()> {
    register_crap_jobs_catalog(lua, state)?;
    Ok(())
}
