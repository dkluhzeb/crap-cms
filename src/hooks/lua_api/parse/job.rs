//! Parsing functions for job definitions from Lua tables.

use std::str::FromStr;

use anyhow::{Result, anyhow, bail};
use cron::Schedule;
use mlua::{FromLua, Lua, LuaSerdeExt, Result as LuaResult, Value};
use serde::Deserialize;

use crate::core::job::JobDefinitionBuilder;
use crate::core::{HookRef, JobDefinition, JobLabels};
use crate::db::query;
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
    /// Cron expression (e.g., `"0 3 * * *"`). When set, the job runs on
    /// this schedule. Accepts both 5-field and 6/7-field forms.
    pub schedule: Option<String>,
    /// Queue name (default: `"default"`).
    pub queue: Option<String>,
    /// Max retry attempts on failure (default: `0`).
    pub retries: Option<u32>,
    /// Seconds before a running job is marked failed (default: `60`).
    pub timeout: Option<u64>,
    /// Max concurrent runs of this job (default: `1`).
    pub concurrency: Option<u32>,
    /// Default scheduling priority for this job. Used when a queue
    /// site doesn't pass an explicit `{ priority = N }`. Higher =
    /// claimed sooner; negative = run only when otherwise idle.
    /// Default: `0`.
    pub priority: Option<i32>,
    /// Skip scheduled run if a previous run is still active (default:
    /// `true`).
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
    // uniform.
    query::validate_slug(slug)?;

    let handler = config
        .handler
        .ok_or_else(|| anyhow!("Job '{slug}' missing required 'handler' field"))?;

    if let Some(ref expr) = config.schedule {
        let normalized = if expr.split_whitespace().count() == 5 {
            format!("0 {expr}")
        } else {
            expr.clone()
        };

        if Schedule::from_str(&normalized).is_err() {
            bail!("Job '{slug}' has invalid cron expression '{expr}'");
        }
    }

    let mut builder = JobDefinitionBuilder::new(slug, handler)
        .queue(config.queue.unwrap_or_else(|| "default".to_string()))
        .timeout(config.timeout.unwrap_or(60))
        .concurrency(config.concurrency.unwrap_or(1))
        .priority(config.priority.unwrap_or(0))
        .skip_if_running(config.skip_if_running.unwrap_or(true))
        .labels(config.labels.unwrap_or_default());

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
    /// underscore) must now be rejected at load.
    #[test]
    fn parse_job_definition_rejects_invalid_slug() {
        let lua = Lua::new();
        for bad in ["my-job", "MyJob", "_hidden", "has space"] {
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
}
