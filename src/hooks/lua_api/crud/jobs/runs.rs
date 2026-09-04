//! `crap.jobs.get_run` / `list_runs` / `cancel_run` — the read and cancel
//! half of the Lua job surface.
//!
//! Without these a hook could queue work (`crap.jobs.queue`) and never look
//! at it again. They are thin wrappers over the SAME
//! `service::jobs::{get_job_run, list_job_runs, cancel_job_run}` chokepoints
//! the gRPC RPCs and MCP tools call, so the job access gate, the
//! queued-bulk visibility rule, and the "only pending runs cancel" rule
//! cannot drift between surfaces.
//!
//! The access check runs through [`LuaWriteHooks`] — the in-VM evaluator —
//! so a hook never re-enters the VM pool, exactly like Lua CRUD.

use std::sync::Arc;

use mlua::{Error::RuntimeError, Lua, Result as LuaResult, Table};

use crate::{
    core::Registry,
    hooks::lua_api::{
        crud::{helpers::hook_user, tx_conn::get_tx_conn},
        parse::deny_unknown_keys,
    },
    service::{self, LuaWriteHooks, ServiceContext, op::wire},
    typegen::lua::{LuaFnSpec, LuaParam, LuaReturn, lua_fn, lua_table},
};

/// Registry handle for the job read/cancel functions.
pub(crate) struct JobsRunsState {
    pub(crate) registry: Arc<Registry>,
}

/// Build the service context for a job read/cancel from inside a VM: the
/// hook's user drives the access gate, and the in-VM hooks evaluate it.
fn job_ctx<'a>(
    lua: &'a Lua,
    conn: &'a dyn crate::db::DbConnection,
    hooks: &'a LuaWriteHooks<'a>,
    user: Option<&'a crate::core::Document>,
) -> ServiceContext<'a> {
    let _ = lua;

    ServiceContext::slug_only("")
        .conn(conn)
        .write_hooks(hooks)
        .user(user)
        .build()
}

/// Convert a job run into the Lua table shape.
fn run_to_table(lua: &Lua, run: &crate::core::job::JobRun) -> LuaResult<Table> {
    let t = lua.create_table()?;

    t.set("id", run.id.clone())?;
    t.set("slug", run.slug.clone())?;
    t.set("status", run.status.as_str())?;
    t.set("queue", run.queue.clone())?;
    t.set("attempt", run.attempt)?;
    t.set("max_attempts", run.max_attempts)?;

    if let Some(result) = run.result.as_deref() {
        t.set("result", result)?;
    }
    if let Some(error) = run.error.as_deref() {
        t.set("error", error)?;
    }
    if let Some(created_at) = run.created_at.as_deref() {
        t.set("created_at", created_at)?;
    }

    Ok(t)
}

/// Look up one job run by id.
#[lua_fn(
    path = "crap.jobs.get_run",
    returns_doc = "The run table (`id`, `slug`, `status`, `queue`, `attempt`, `max_attempts`, and `result` / `error` / `created_at` when set), or nil when it does not exist or is not visible.",
    auto_tx_read
)]
fn jobs_get_run(
    state: &JobsRunsState,
    lua: &Lua,
    #[lua(doc = "Job run id (as returned by `crap.jobs.queue`).")] id: String,
) -> LuaResult<Option<Table>> {
    let conn = get_tx_conn(lua)?;
    let user = hook_user(lua);
    let hooks = LuaWriteHooks::builder(lua).build();
    let ctx = job_ctx(lua, conn, &hooks, user.as_ref());

    let run = service::jobs::get_job_run(&ctx, state.registry.as_ref(), &id)
        .map_err(|e| RuntimeError(format!("jobs.get_run: {e}")))?;

    run.as_ref().map(|r| run_to_table(lua, r)).transpose()
}

/// List recent job runs, newest first.
#[lua_fn(
    path = "crap.jobs.list_runs",
    returns_doc = "A table with `runs` (array of run tables) and `total`.",
    auto_tx_read
)]
fn jobs_list_runs(
    state: &JobsRunsState,
    lua: &Lua,
    #[lua(
        doc = "Options table. Supports `slug` (string — only this job's runs), `status` (`\"pending\"` | `\"running\"` | `\"completed\"` | `\"failed\"` | `\"stale\"`), `limit` (integer, default 50) and `offset` (integer, default 0)."
    )]
    opts: Option<Table>,
) -> LuaResult<Table> {
    if let Some(opts) = opts.as_ref() {
        // Accepted keys come from the wire model — see `jobs.queue`.
        let allowed = wire::job_op("list_job_runs")
            .expect("list_job_runs is modeled")
            .lua_option_keys(&[]);
        deny_unknown_keys(opts, "jobs.list_runs options", &allowed)
            .map_err(|e| RuntimeError(format!("jobs.list_runs: {e}")))?;
    }

    let slug: Option<String> = opts.as_ref().and_then(|o| o.get("slug").ok());
    let status: Option<String> = opts.as_ref().and_then(|o| o.get("status").ok());
    let limit: i64 = opts
        .as_ref()
        .and_then(|o| o.get::<Option<i64>>("limit").ok().flatten())
        .unwrap_or(50)
        .max(0);
    let offset: i64 = opts
        .as_ref()
        .and_then(|o| o.get::<Option<i64>>("offset").ok().flatten())
        .unwrap_or(0)
        .max(0);

    let conn = get_tx_conn(lua)?;
    let user = hook_user(lua);
    let hooks = LuaWriteHooks::builder(lua).build();
    let ctx = job_ctx(lua, conn, &hooks, user.as_ref());

    let page = service::jobs::list_job_runs(
        &ctx,
        &service::jobs::ListJobRunsInput {
            registry: state.registry.as_ref(),
            slug: slug.as_deref(),
            status: status.as_deref(),
            limit,
            offset,
        },
    )
    .map_err(|e| RuntimeError(format!("jobs.list_runs: {e}")))?;

    let out = lua.create_table()?;
    let runs = lua.create_table()?;

    for (i, run) in page.docs.iter().enumerate() {
        runs.raw_set(i + 1, run_to_table(lua, run)?)?;
    }

    out.set("runs", runs)?;
    out.set("total", page.total)?;

    Ok(out)
}

/// Cancel a job run that has not been claimed yet.
#[lua_fn(
    path = "crap.jobs.cancel_run",
    returns_doc = "True when a pending run was cancelled; false when it does not exist, is not visible, or has already been claimed.",
    auto_tx
)]
fn jobs_cancel_run(
    state: &JobsRunsState,
    lua: &Lua,
    #[lua(doc = "Job run id to cancel.")] id: String,
) -> LuaResult<bool> {
    let conn = get_tx_conn(lua)?;
    let user = hook_user(lua);
    let hooks = LuaWriteHooks::builder(lua).build();
    let ctx = job_ctx(lua, conn, &hooks, user.as_ref());

    service::jobs::cancel_job_run(&ctx, state.registry.as_ref(), &id)
        .map_err(|e| RuntimeError(format!("jobs.cancel_run: {e}")))
}

lua_table! {
    name: crap_jobs_runs,
    path: "crap.jobs",
    state: JobsRunsState,
    fns: [jobs_get_run, jobs_list_runs, jobs_cancel_run],
}

/// Register the job read/cancel functions on `crap.jobs`.
#[cfg(not(tarpaulin_include))]
pub(crate) fn register_jobs_runs(lua: &Lua, state: JobsRunsState) -> anyhow::Result<()> {
    register_crap_jobs_runs(lua, state)?;
    Ok(())
}
