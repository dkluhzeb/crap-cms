//! Parsing functions for job definitions from Lua tables.

use anyhow::{Result, anyhow, bail};
use mlua::{FromLua, Lua, LuaSerdeExt, Result as LuaResult, Value};
use serde::Deserialize;

use crate::core::job::JobDefinitionBuilder;
use crate::core::{HookRef, JobDefinition, JobLabels};
use crate::db::query;
use crate::scheduler::parse_cron;
use crate::typegen::lua::LuaAnnotation;

/// Typed `config` table passed to `crap.jobs.define(slug, config)`.
#[derive(Default, Deserialize, LuaAnnotation)]
#[serde(default, deny_unknown_fields)]
#[lua(class = "crap.JobDefinitionConfig")]
pub struct JobDefinitionConfig {
    /// Lua function ref for the job handler (required, e.g.,
    /// `"jobs.cleanup.run"`). May carry per-definition options exposed to the
    /// handler as `ctx.options`.
    #[lua(ty = "string | crap.HookRef", optional)]
    pub handler: Option<HookRef>,
    /// Cron expression (e.g., `"0 3 * * *"`), evaluated in UTC. When set,
    /// the job runs on this schedule. Accepts both 5-field and 6/7-field
    /// forms.
    pub schedule: Option<String>,
    /// Queue name (default: `"default"`).
    pub queue: Option<String>,
    /// Max retry attempts on failure. Omit to inherit the queue's
    /// `[jobs.queues.<queue>] retries` (else `0`).
    pub retries: Option<u32>,
    /// Wall-clock budget in seconds (default: `60`, minimum `1`). Once it
    /// passes, the handler is stopped at its next Lua instruction batch or
    /// database / HTTP / email call, the operation in flight is rolled back,
    /// and the run is failed (and retried if attempts remain).
    pub timeout: Option<u64>,
    /// Max concurrent runs of this job (default: `1`).
    pub concurrency: Option<u32>,
    /// Default scheduling priority for this job. Used when a queue
    /// site doesn't pass an explicit `{ priority = N }`. Higher =
    /// claimed sooner; negative = run only when otherwise idle.
    /// Default: `0`.
    pub priority: Option<i32>,
    /// Skip a scheduled run while a previous run of this job is still
    /// queued or running (default: `true`).
    pub skip_if_running: Option<bool>,
    /// Display labels for the admin UI.
    #[lua(ty = "crap.JobLabels", optional)]
    pub labels: Option<JobLabels>,
    /// Lua function ref for access control on gRPC/CLI trigger.
    #[lua(ty = "string | crap.HookRef", optional)]
    pub access: Option<HookRef>,
}

impl FromLua for JobDefinitionConfig {
    fn from_lua(value: Value, lua: &Lua) -> LuaResult<Self> {
        match value {
            Value::Nil => Ok(Self::default()),
            other => lua.from_value(other),
        }
    }
}

/// Parse a `JobDefinitionConfig` into a `JobDefinition`.
///
/// # Errors
///
/// Returns an error if `handler` is missing or the cron expression is
/// invalid.
pub fn parse_job_definition(slug: &str, config: JobDefinitionConfig) -> Result<JobDefinition> {
    // Validate the slug like collections and globals do — the job slug is a
    // stored value (`_crap_jobs.slug`), so this is a consistency guarantee
    // rather than an injection fix, but it keeps every registration surface
    // uniform. The reserved-prefix reject is *inert* for jobs (a job slug never
    // builds a `{op}_{slug}` MCP tool name, so `many_`/`by_id_` can't collide) —
    // applied purely so every slug intake runs the identical validation pair,
    // with no behavioral downside.
    query::validate_slug(slug)?;
    query::reject_reserved_tool_prefix(slug)?;

    let handler = config
        .handler
        .ok_or_else(|| anyhow!("Job '{slug}' missing required 'handler' field"))?;

    // Parsed through the scheduler's own entry point, so a schedule accepted
    // here is exactly a schedule the scheduler will later fire on — including
    // the crontab day-of-week numbering it translates.
    if let Some(expr) = config.schedule.as_deref()
        && let Err(e) = parse_cron(expr)
    {
        bail!("Job '{slug}' has invalid cron expression '{expr}': {e}");
    }

    // Apply each field only when the operator set it, letting the builder's
    // `JobDefinition::default()` supply the fallback — so the scheduling defaults
    // (queue/timeout/concurrency/priority/skip_if_running) live in exactly one
    // place and a Lua-defined job never keeps a stale literal if a default moves.
    let mut builder = JobDefinitionBuilder::new(slug, handler);

    if let Some(queue) = config.queue {
        builder = builder.queue(queue);
    }
    if let Some(timeout) = config.timeout {
        // `0` is not "no timeout": the handler would be stopped the moment it
        // starts, on every attempt. Refuse it here rather than let the job
        // fail at run time.
        if timeout == 0 {
            bail!(
                "Job '{slug}' has `timeout = 0` — a job's timeout must be at least 1 \
                 second (omit it for the default of 60)"
            );
        }

        builder = builder.timeout(timeout);
    }
    if let Some(concurrency) = config.concurrency {
        builder = builder.concurrency(concurrency);
    }
    if let Some(priority) = config.priority {
        builder = builder.priority(priority);
    }
    if let Some(skip_if_running) = config.skip_if_running {
        builder = builder.skip_if_running(skip_if_running);
    }
    if let Some(labels) = config.labels {
        builder = builder.labels(labels);
    }

    // Retries: pass through only when the operator set it. `None` leaves
    // the field unset on the JobDefinition so that
    // `effective_max_attempts` can later fall back to
    // `[jobs.queues.<queue>] retries`.
    if let Some(n) = config.retries {
        builder = builder.retries(n);
    }

    if let Some(s) = config.schedule {
        builder = builder.schedule(s);
    }

    if let Some(a) = config.access {
        builder = builder.access(a);
    }

    Ok(builder.build())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mlua::Lua;

    fn from_lua_table(lua: &Lua, src: &str) -> JobDefinitionConfig {
        let table: mlua::Table = lua.load(src).eval().unwrap();
        JobDefinitionConfig::from_lua(Value::Table(table), lua).unwrap()
    }

    #[test]
    fn test_parse_job_definition_minimal() {
        let lua = Lua::new();
        let cfg = from_lua_table(&lua, r#"return { handler = "jobs.my_job.run" }"#);

        let job = parse_job_definition("my_job", cfg).unwrap();
        assert_eq!(job.slug, "my_job");
        assert_eq!(job.handler.reference(), "jobs.my_job.run");
        assert!(job.schedule.is_none());
        assert_eq!(job.queue, "default");
        assert_eq!(
            job.retries, None,
            "retries omitted in define → None on JobDefinition (queue default applies at queue-time)"
        );
        assert_eq!(job.timeout, 60);
        assert_eq!(job.concurrency, 1);
        assert!(job.skip_if_running);
        assert!(job.access.is_none());
    }

    #[test]
    fn test_parse_job_definition_full() {
        let lua = Lua::new();
        let cfg = from_lua_table(
            &lua,
            r#"return {
                handler = "jobs.sync.run",
                schedule = "*/5 * * * *",
                queue = "sync",
                retries = 3,
                timeout = 300,
                concurrency = 2,
                skip_if_running = false,
                access = "access.admin_only",
                labels = { singular = "Sync Job" },
            }"#,
        );

        let job = parse_job_definition("sync", cfg).unwrap();
        assert_eq!(job.slug, "sync");
        assert_eq!(job.handler.reference(), "jobs.sync.run");
        assert_eq!(job.schedule.as_deref(), Some("*/5 * * * *"));
        assert_eq!(job.queue, "sync");
        assert_eq!(job.retries, Some(3));
        assert_eq!(job.timeout, 300);
        assert_eq!(job.concurrency, 2);
        assert!(!job.skip_if_running);
        assert_eq!(
            job.access.as_ref().map(HookRef::reference),
            Some("access.admin_only")
        );
        assert_eq!(job.labels.singular.as_deref(), Some("Sync Job"));
    }

    #[test]
    fn test_parse_job_definition_missing_handler() {
        let cfg = JobDefinitionConfig::default();
        let result = parse_job_definition("bad_job", cfg);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("missing required 'handler'")
        );
    }

    /// Regression: `timeout = 0` was accepted, and every run of the job then
    /// timed out the moment it was spawned — while its handler kept running
    /// next to the retry. A timeout must be at least one second.
    #[test]
    fn parse_job_definition_rejects_a_zero_timeout() {
        let lua = Lua::new();
        let cfg = from_lua_table(&lua, r#"return { handler = "jobs.x.run", timeout = 0 }"#);

        let err = parse_job_definition("zero", cfg)
            .expect_err("timeout = 0 must be rejected")
            .to_string();

        assert!(err.contains("timeout = 0"), "unexpected: {err}");
    }

    #[test]
    fn test_parse_job_definition_invalid_cron() {
        let lua = Lua::new();
        let cfg = from_lua_table(
            &lua,
            r#"return { handler = "jobs.bad.run", schedule = "not a cron" }"#,
        );
        let result = parse_job_definition("bad_job", cfg);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("invalid cron expression")
        );
    }

    /// Regression: define-time validation used to prepend the seconds field
    /// and hand the rest straight to the `cron` crate, whose day-of-week
    /// numbering starts at Sunday = 1. The standard crontab spelling of
    /// Sunday (`0`) was therefore rejected outright at definition time.
    #[test]
    fn parse_job_definition_accepts_crontab_day_of_week() {
        let lua = Lua::new();

        for schedule in ["0 3 * * 0", "0 3 * * 7", "0 8 * * 1-5", "0 8 * * MON-FRI"] {
            let cfg = from_lua_table(
                &lua,
                &format!(r#"return {{ handler = "jobs.x.run", schedule = "{schedule}" }}"#),
            );

            assert!(
                parse_job_definition("weekly", cfg).is_ok(),
                "crontab schedule '{schedule}' must be accepted"
            );
        }
    }

    #[test]
    fn test_parse_job_definition_7_field_cron() {
        let lua = Lua::new();
        let cfg = from_lua_table(
            &lua,
            r#"return { handler = "jobs.hourly.run", schedule = "0 0 * * * * *" }"#,
        );
        let job = parse_job_definition("hourly", cfg).unwrap();
        assert_eq!(job.schedule.as_deref(), Some("0 0 * * * * *"));
    }

    /// Regression: `crap.jobs.define` never validated the slug, unlike
    /// collections and globals. An invalid slug (hyphen, uppercase, leading
    /// underscore) must now be rejected at load. The leading-underscore rule
    /// also prevents a user job from colliding with a `__`-prefixed system
    /// pseudo-cron slug (e.g. `__retention_purge`) in `_crap_cron_fired`.
    #[test]
    fn parse_job_definition_rejects_invalid_slug() {
        let lua = Lua::new();
        for bad in [
            "my-job",
            "MyJob",
            "_hidden",
            "__retention_purge",
            "has space",
        ] {
            let cfg = from_lua_table(&lua, r#"return { handler = "jobs.x.run" }"#);
            let result = parse_job_definition(bad, cfg);
            assert!(
                result.is_err(),
                "slug '{bad}' should be rejected by validate_slug"
            );
        }

        // A valid slug still parses.
        let cfg = from_lua_table(&lua, r#"return { handler = "jobs.x.run" }"#);
        assert!(parse_job_definition("my_job", cfg).is_ok());
    }

    /// Consistency: jobs run the same reserved-tool-prefix reject as collections
    /// and globals. Inert for jobs (a job slug never builds an MCP tool name),
    /// but every slug intake now validates identically.
    #[test]
    fn parse_job_definition_rejects_reserved_tool_prefix() {
        let lua = Lua::new();
        for bad in ["many_things", "by_id_lookup"] {
            let cfg = from_lua_table(&lua, r#"return { handler = "jobs.x.run" }"#);
            assert!(
                parse_job_definition(bad, cfg).is_err(),
                "slug '{bad}' should be rejected by reject_reserved_tool_prefix"
            );
        }
    }
}
