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
        integer::opt_integer,
        parse::{deny_unknown_keys, get_string_strict},
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
    let hooks = LuaWriteHooks::builder(lua, state.registry.as_ref()).build();
    let ctx = job_ctx(lua, conn, &hooks, user.as_ref());

    let run = service::jobs::get_job_run(&ctx, state.registry.as_ref(), &id)
        .map_err(|e| RuntimeError(format!("jobs.get_run: {e}")))?;

    run.as_ref().map(|r| run_to_table(lua, r)).transpose()
}

/// The typed `crap.jobs.list_runs` options. Each option is read strictly: a
/// wrong-typed value is an error naming the key, never a silent fallback
/// (`{ slug = {"digest"} }` used to list every readable job).
#[derive(Debug)]
struct ListRunsOptions {
    slug: Option<String>,
    status: Option<String>,
    limit: i64,
    offset: i64,
}

impl Default for ListRunsOptions {
    fn default() -> Self {
        Self {
            slug: None,
            status: None,
            limit: 50,
            offset: 0,
        }
    }
}

/// Parse the options table; unknown keys are rejected against the wire
/// model (see `jobs.queue`), values against their declared types.
fn parse_list_runs_options(lua: &Lua, opts: Option<&Table>) -> LuaResult<ListRunsOptions> {
    const CONTEXT: &str = "jobs.list_runs options";

    let Some(opts) = opts else {
        return Ok(ListRunsOptions::default());
    };

    let allowed = wire::job_op("list_job_runs")
        .expect("list_job_runs is modeled")
        .lua_option_keys(&[]);
    deny_unknown_keys(opts, CONTEXT, &allowed)
        .map_err(|e| RuntimeError(format!("jobs.list_runs: {e}")))?;

    let defaults = ListRunsOptions::default();

    Ok(ListRunsOptions {
        slug: get_string_strict(opts, "slug", CONTEXT)?,
        status: get_string_strict(opts, "status", CONTEXT)?,
        limit: opt_integer(lua, opts, "limit", CONTEXT)?
            .unwrap_or(defaults.limit)
            .max(0),
        offset: opt_integer(lua, opts, "offset", CONTEXT)?
            .unwrap_or(defaults.offset)
            .max(0),
    })
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
    let ListRunsOptions {
        slug,
        status,
        limit,
        offset,
    } = parse_list_runs_options(lua, opts.as_ref())?;

    let conn = get_tx_conn(lua)?;
    let user = hook_user(lua);
    let hooks = LuaWriteHooks::builder(lua, state.registry.as_ref()).build();
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
    let hooks = LuaWriteHooks::builder(lua, state.registry.as_ref()).build();
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

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(lua: &Lua, src: &str) -> Table {
        lua.load(src).eval().unwrap()
    }

    #[test]
    fn absent_options_use_the_defaults() {
        let lua = Lua::new();
        let parsed = parse_list_runs_options(&lua, None).unwrap();
        assert_eq!(parsed.slug, None);
        assert_eq!(parsed.status, None);
        assert_eq!(parsed.limit, 50);
        assert_eq!(parsed.offset, 0);
    }

    #[test]
    fn typed_options_are_read() {
        let lua = Lua::new();
        let t = opts(
            &lua,
            "return { slug = 'digest', status = 'failed', limit = 2^4, offset = 3 }",
        );
        let parsed = parse_list_runs_options(&lua, Some(&t)).unwrap();
        assert_eq!(parsed.slug.as_deref(), Some("digest"));
        assert_eq!(parsed.status.as_deref(), Some("failed"));
        assert_eq!(parsed.limit, 16, "a whole-valued float is an integer");
        assert_eq!(parsed.offset, 3);
    }

    /// `{ slug = {} }` used to be `.ok()`-ed away and list EVERY readable
    /// job; it must error naming the key.
    #[test]
    fn wrong_typed_slug_errors_naming_the_key() {
        let lua = Lua::new();
        let t = opts(&lua, "return { slug = {} }");
        let err = parse_list_runs_options(&lua, Some(&t))
            .unwrap_err()
            .to_string();
        assert!(err.contains("'slug'"), "names the key: {err}");
        assert!(err.contains("must be a string"), "{err}");
    }

    /// `{ limit = "x" }` used to silently fall back to 50.
    #[test]
    fn wrong_typed_limit_errors() {
        let lua = Lua::new();
        let t = opts(&lua, "return { limit = 'x' }");
        let err = parse_list_runs_options(&lua, Some(&t))
            .unwrap_err()
            .to_string();
        assert!(err.contains("'limit' must be an integer"), "{err}");

        let t = opts(&lua, "return { offset = 1.5 }");
        let err = parse_list_runs_options(&lua, Some(&t))
            .unwrap_err()
            .to_string();
        assert!(err.contains("'offset' must be an integer"), "{err}");
    }

    #[test]
    fn negative_limit_and_offset_clamp_to_zero() {
        let lua = Lua::new();
        let t = opts(&lua, "return { limit = -5, offset = -1 }");
        let parsed = parse_list_runs_options(&lua, Some(&t)).unwrap();
        assert_eq!(parsed.limit, 0);
        assert_eq!(parsed.offset, 0);
    }

    #[test]
    fn unknown_key_is_rejected() {
        let lua = Lua::new();
        let t = opts(&lua, "return { slugg = 'digest' }");
        let err = parse_list_runs_options(&lua, Some(&t))
            .unwrap_err()
            .to_string();
        assert!(err.contains("slugg"), "{err}");
    }
}
