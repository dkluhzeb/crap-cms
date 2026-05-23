//! Parsing functions for job definitions from Lua tables.

use std::str::FromStr;

use anyhow::{Result, anyhow, bail};
use cron::Schedule;
use mlua::{FromLua, Lua, LuaSerdeExt, Result as LuaResult, Value};
use serde::Deserialize;

use crate::core::job::JobDefinitionBuilder;
use crate::core::{JobDefinition, JobLabels};
use crate::typegen::lua::LuaAnnotation;

/// Typed `config` table passed to `crap.jobs.define(slug, config)`.
#[derive(Default, Deserialize, LuaAnnotation)]
#[serde(default)]
#[lua(class = "crap.JobDefinitionConfig")]
pub struct JobDefinitionConfig {
    /// Lua function ref for the job handler (required, e.g.,
    /// `"jobs.cleanup.run"`).
    pub handler: Option<String>,
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
    pub access: Option<String>,
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
        .retries(config.retries.unwrap_or(0))
        .timeout(config.timeout.unwrap_or(60))
        .concurrency(config.concurrency.unwrap_or(1))
        .priority(config.priority.unwrap_or(0))
        .skip_if_running(config.skip_if_running.unwrap_or(true))
        .labels(config.labels.unwrap_or_default());

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

        let job = parse_job_definition("my-job", cfg).unwrap();
        assert_eq!(job.slug, "my-job");
        assert_eq!(job.handler, "jobs.my_job.run");
        assert!(job.schedule.is_none());
        assert_eq!(job.queue, "default");
        assert_eq!(job.retries, 0);
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
        assert_eq!(job.handler, "jobs.sync.run");
        assert_eq!(job.schedule.as_deref(), Some("*/5 * * * *"));
        assert_eq!(job.queue, "sync");
        assert_eq!(job.retries, 3);
        assert_eq!(job.timeout, 300);
        assert_eq!(job.concurrency, 2);
        assert!(!job.skip_if_running);
        assert_eq!(job.access.as_deref(), Some("access.admin_only"));
        assert_eq!(job.labels.singular.as_deref(), Some("Sync Job"));
    }

    #[test]
    fn test_parse_job_definition_missing_handler() {
        let cfg = JobDefinitionConfig::default();
        let result = parse_job_definition("bad-job", cfg);
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
        let result = parse_job_definition("bad-job", cfg);
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
}
