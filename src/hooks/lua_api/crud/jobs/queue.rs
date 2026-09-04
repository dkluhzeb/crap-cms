//! Registration of `crap.jobs.queue` Lua function.

use std::collections::HashMap;
use std::sync::Arc;

use crate::hooks::lua_api::utils::lua_err;
use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Result as LuaResult, Table, Value};

use crate::config::parse_duration_string;
use crate::core::Registry;
use crate::hooks::lua_api;
use crate::hooks::lua_api::crud::{get_tx_conn, helpers::hook_user};
use crate::hooks::lua_api::parse::deny_unknown_keys;
use crate::service::op::wire;
use crate::service::{self, LuaWriteHooks, ServiceContext};
use crate::typegen::lua::{LuaFnSpec, LuaParam, LuaReturn, lua_fn, lua_table};

/// State threaded into `crap.jobs.queue` — the loaded `Registry` plus a
/// snapshot of `[jobs.queues.<name>] retries` so jobs defined without
/// an explicit `retries` field inherit the operator's queue-level
/// default at queue time. Only queues with a `Some(N)` retries entry
/// appear in the map.
pub(crate) struct JobsQueueState {
    registry: Arc<Registry>,
    queue_retries: HashMap<String, u32>,
}

impl JobsQueueState {
    pub(crate) fn new(registry: Arc<Registry>, queue_retries: HashMap<String, u32>) -> Self {
        Self {
            registry,
            queue_retries,
        }
    }
}

/// Queue a job for background execution. Returns the job run ID.
/// Only available inside hooks with transaction context.
#[lua_fn(
    path = "crap.jobs.queue",
    returns_doc = "The queued job run ID.",
    auto_tx
)]
fn jobs_queue(
    state: &JobsQueueState,
    lua: &Lua,
    #[lua(doc = "Job slug (must be previously defined).")] slug: String,
    #[lua(doc = "Input data passed to the handler (default: {}).")] data: Option<Table>,
    #[lua(
        doc = "Options table. Supports `priority` (integer, default falls back to the job definition's priority; higher = sooner), `delay` (integer seconds or duration string `\"5m\"` / `\"30s\"` / `\"1h\"`; default 0 = immediate), and `unique` (string dedup key — if another pending/running job has the same slug+unique key, returns its id instead of inserting a duplicate)."
    )]
    opts: Option<Table>,
) -> LuaResult<String> {
    queue_job_inner(lua, state, &slug, data, opts.as_ref())
}

lua_table! {
    name: crap_jobs_queue,
    path: "crap.jobs",
    state: JobsQueueState,
    fns: [jobs_queue],
}

/// Register `crap.jobs.queue(slug, data?)`. Parent `crap.jobs` table
/// must already exist (populated by `register_jobs_init` or
/// `register_jobs_pool_init`).
#[cfg(not(tarpaulin_include))]
pub(crate) fn register_jobs_queue(lua: &Lua, _table: &Table, state: JobsQueueState) -> Result<()> {
    register_crap_jobs_queue(lua, state)?;
    Ok(())
}

/// Core logic for `crap.jobs.queue`. Extracted so the `#[lua_fn]` wrapper
/// stays thin and the hot path is easy to read on its own.
fn queue_job_inner(
    lua: &Lua,
    state: &JobsQueueState,
    slug: &str,
    data: Option<Table>,
    opts: Option<&Table>,
) -> LuaResult<String> {
    let reg = state.registry.as_ref();
    let conn = get_tx_conn(lua)?;

    // Reject unknown option keys (parity with the strict schema config):
    // a typo like `{ priorty = 5 }` errors loudly instead of being silently
    // ignored. The accepted keys come from the wire model, so this surface
    // cannot drift from the others (`slug` and `data` are positional).
    if let Some(opts) = opts {
        let allowed = wire::job_op("trigger_job")
            .expect("trigger_job is modeled")
            .lua_option_keys(&["slug", "data"]);
        deny_unknown_keys(opts, "jobs.queue options", &allowed).map_err(lua_err)?;
    }

    let job_def = reg
        .get_job(slug)
        .cloned()
        .ok_or_else(|| RuntimeError(format!("Job '{slug}' not defined")))?;

    // Per-enqueue `priority` overrides the definition's default;
    // absent → fall back to `JobDefinition::priority`. Wrong-typed
    // values produce a clear error rather than silently falling back.
    let priority: i32 = parse_priority_opt(opts)?.unwrap_or(job_def.priority);

    // `delay` accepts either an integer (seconds) or a duration string
    // (`"5m"`, `"30s"`, `"1h"`). 0 / absent = no delay.
    let delay_secs: u64 = parse_delay_opt(opts)?;

    let unique_key: Option<String> = parse_unique_opt(opts)?;

    let data_json = match data {
        Some(tbl) => {
            let json_val = lua_api::lua_to_json(&Value::Table(tbl))?;

            serde_json::to_string(&json_val)
                .map_err(|e| RuntimeError(format!("JSON error: {e:#}")))?
        }
        None => "{}".to_string(),
    };

    let queue_retries = state.queue_retries.get(&job_def.queue).copied();

    // Through the ONE queue chokepoint (`service::jobs::queue_job`) — the
    // same access evaluation, payload contract, and delay/unique semantics
    // as gRPC and MCP. `LuaWriteHooks` runs the access function in THIS VM,
    // so a hook queuing a job never re-enters the VM pool.
    let user = hook_user(lua);
    let hooks = LuaWriteHooks::builder(lua).build();
    let ctx = ServiceContext::slug_only(slug)
        .conn(conn)
        .write_hooks(&hooks)
        .user(user.as_ref())
        .build();

    let run = service::jobs::queue_job(
        &ctx,
        &service::jobs::QueueJobInput {
            job_def: &job_def,
            data: Some(&data_json),
            scheduled_by: "hook",
            priority,
            queue_retries,
            delay_secs,
            unique_key: unique_key.as_deref(),
        },
    )
    .map_err(|e| RuntimeError(format!("{e}")))?;

    Ok(run.id)
}

/// Parse the `priority` option from a `crap.jobs.queue` opts table.
/// Returns `Ok(None)` when absent so the caller can fall back to the
/// job definition's default. Wrong-typed values produce a clear error.
fn parse_priority_opt(opts: Option<&Table>) -> LuaResult<Option<i32>> {
    let Some(opts) = opts else { return Ok(None) };
    match opts.get::<Value>("priority")? {
        Value::Nil => Ok(None),
        Value::Integer(n) => i32::try_from(n)
            .map(Some)
            .map_err(|_| RuntimeError(format!("priority value out of i32 range: {n}"))),
        other => Err(RuntimeError(format!(
            "priority must be an integer (or omitted); got {}",
            other.type_name()
        ))),
    }
}

/// Parse the `delay` option from a `crap.jobs.queue` opts table.
/// Accepts an integer (seconds) or a duration string
/// (`"5m"`, `"30s"`, `"1h"`). Missing / nil → `0`. Invalid string or
/// wrong type → clear runtime error.
fn parse_delay_opt(opts: Option<&Table>) -> LuaResult<u64> {
    let Some(opts) = opts else { return Ok(0) };
    match opts.get::<Value>("delay")? {
        Value::Nil => Ok(0),
        Value::Integer(n) => {
            if n < 0 {
                return Err(RuntimeError(format!("delay must be non-negative; got {n}")));
            }
            u64::try_from(n).map_err(|_| RuntimeError(format!("delay value out of range: {n}")))
        }
        Value::String(s) => {
            let s = s.to_str()?.to_string();
            parse_duration_string(&s).ok_or_else(|| {
                RuntimeError(format!(
                    "delay '{s}' is not a valid duration (expected seconds integer or '30s' / '5m' / '1h' / '7d')"
                ))
            })
        }
        other => Err(RuntimeError(format!(
            "delay must be a non-negative integer or duration string; got {}",
            other.type_name()
        ))),
    }
}

/// Parse the `unique` option from a `crap.jobs.queue` opts table.
/// Returns `Ok(None)` when absent; rejects non-string values and
/// empty strings.
fn parse_unique_opt(opts: Option<&Table>) -> LuaResult<Option<String>> {
    let Some(opts) = opts else { return Ok(None) };
    match opts.get::<Value>("unique")? {
        Value::Nil => Ok(None),
        Value::String(s) => {
            let s = s.to_str()?.to_string();
            if s.is_empty() {
                return Err(RuntimeError(
                    "unique key must be non-empty (or omit the option)".into(),
                ));
            }
            Ok(Some(s))
        }
        other => Err(RuntimeError(format!(
            "unique must be a string (or omitted); got {}",
            other.type_name()
        ))),
    }
}

#[cfg(test)]
#[allow(clippy::missing_panics_doc)]
mod tests {
    use super::*;
    use mlua::Lua;

    fn opts_from_lua(lua: &Lua, src: &str) -> Table {
        lua.load(src).eval().unwrap()
    }

    // ── parse_priority_opt ─────────────────────────────────────────

    #[test]
    fn priority_absent_returns_none() {
        let lua = Lua::new();
        let t = opts_from_lua(&lua, "return {}");
        assert_eq!(parse_priority_opt(Some(&t)).unwrap(), None);
    }

    #[test]
    fn priority_integer_returns_some() {
        let lua = Lua::new();
        let t = opts_from_lua(&lua, "return { priority = 5 }");
        assert_eq!(parse_priority_opt(Some(&t)).unwrap(), Some(5));
    }

    #[test]
    fn priority_negative_integer_ok() {
        let lua = Lua::new();
        let t = opts_from_lua(&lua, "return { priority = -10 }");
        assert_eq!(parse_priority_opt(Some(&t)).unwrap(), Some(-10));
    }

    #[test]
    fn priority_string_errors() {
        let lua = Lua::new();
        let t = opts_from_lua(&lua, "return { priority = 'high' }");
        let err = parse_priority_opt(Some(&t)).unwrap_err().to_string();
        assert!(err.contains("priority must be an integer"), "got: {err}");
    }

    // ── strict opts (unknown-key rejection) ───────────────────────

    /// Regression: `crap.jobs.queue` rejects unknown option keys instead
    /// of silently ignoring them. A typo'd `priorty` must error and
    /// suggest the intended key.
    #[test]
    fn unknown_opts_key_rejected_with_suggestion() {
        let lua = Lua::new();
        let t = opts_from_lua(&lua, "return { priorty = 5 }");
        let err = deny_unknown_keys(&t, "jobs.queue options", &["priority", "delay", "unique"])
            .unwrap_err()
            .to_string();
        assert!(err.contains("priorty"), "names the bad key: {err}");
        assert!(err.contains("priority"), "suggests the real key: {err}");
    }

    #[test]
    fn known_opts_keys_accepted() {
        let lua = Lua::new();
        let t = opts_from_lua(&lua, "return { priority = 1, delay = '5m', unique = 'k' }");
        assert!(
            deny_unknown_keys(&t, "jobs.queue options", &["priority", "delay", "unique"]).is_ok()
        );
    }

    // ── parse_delay_opt ────────────────────────────────────────────

    #[test]
    fn delay_absent_returns_zero() {
        let lua = Lua::new();
        let t = opts_from_lua(&lua, "return {}");
        assert_eq!(parse_delay_opt(Some(&t)).unwrap(), 0);
    }

    #[test]
    fn delay_integer_seconds() {
        let lua = Lua::new();
        let t = opts_from_lua(&lua, "return { delay = 30 }");
        assert_eq!(parse_delay_opt(Some(&t)).unwrap(), 30);
    }

    #[test]
    fn delay_duration_string_minutes() {
        let lua = Lua::new();
        let t = opts_from_lua(&lua, "return { delay = '5m' }");
        assert_eq!(parse_delay_opt(Some(&t)).unwrap(), 300);
    }

    #[test]
    fn delay_negative_integer_errors() {
        let lua = Lua::new();
        let t = opts_from_lua(&lua, "return { delay = -5 }");
        let err = parse_delay_opt(Some(&t)).unwrap_err().to_string();
        assert!(err.contains("non-negative"), "got: {err}");
    }

    #[test]
    fn delay_bogus_string_errors() {
        let lua = Lua::new();
        let t = opts_from_lua(&lua, "return { delay = 'bogus' }");
        let err = parse_delay_opt(Some(&t)).unwrap_err().to_string();
        assert!(err.contains("not a valid duration"), "got: {err}");
    }

    #[test]
    fn delay_invalid_suffix_errors() {
        let lua = Lua::new();
        let t = opts_from_lua(&lua, "return { delay = '5x' }");
        let err = parse_delay_opt(Some(&t)).unwrap_err().to_string();
        assert!(err.contains("not a valid duration"), "got: {err}");
    }

    #[test]
    fn delay_wrong_type_errors() {
        let lua = Lua::new();
        let t = opts_from_lua(&lua, "return { delay = {} }");
        let err = parse_delay_opt(Some(&t)).unwrap_err().to_string();
        assert!(
            err.contains("must be a non-negative integer or duration string"),
            "got: {err}"
        );
    }

    // ── parse_unique_opt ───────────────────────────────────────────

    #[test]
    fn unique_absent_returns_none() {
        let lua = Lua::new();
        let t = opts_from_lua(&lua, "return {}");
        assert_eq!(parse_unique_opt(Some(&t)).unwrap(), None);
    }

    #[test]
    fn unique_valid_string() {
        let lua = Lua::new();
        let t = opts_from_lua(&lua, "return { unique = 'user:42' }");
        assert_eq!(
            parse_unique_opt(Some(&t)).unwrap(),
            Some("user:42".to_string())
        );
    }

    #[test]
    fn unique_empty_string_errors() {
        let lua = Lua::new();
        let t = opts_from_lua(&lua, "return { unique = '' }");
        let err = parse_unique_opt(Some(&t)).unwrap_err().to_string();
        assert!(err.contains("non-empty"), "got: {err}");
    }

    #[test]
    fn unique_number_errors() {
        let lua = Lua::new();
        let t = opts_from_lua(&lua, "return { unique = 42 }");
        let err = parse_unique_opt(Some(&t)).unwrap_err().to_string();
        assert!(err.contains("must be a string"), "got: {err}");
    }
}
