//! Parsing functions for job definitions from Lua tables.

use std::str::FromStr;

use anyhow::{Result, anyhow, bail};
use cron::Schedule;
use mlua::Table;

use crate::core::job::{JobDefinition, JobDefinitionBuilder, JobLabels};

use super::helpers::*;

/// Parse a Lua table into a `JobDefinition`.
pub fn parse_job_definition(slug: &str, config: &Table) -> Result<JobDefinition> {
    let handler = get_string(config, "handler")
        .ok_or_else(|| anyhow!("Job '{}' missing required 'handler' field", slug))?;

    let schedule = get_string(config, "schedule");

    // Validate cron expression early (the cron crate needs 6-7 fields with seconds;
    // we accept standard 5-field expressions and normalize by prepending "0")
    if let Some(ref expr) = schedule {
        let normalized = if expr.split_whitespace().count() == 5 {
            format!("0 {expr}")
        } else {
            expr.clone()
        };

        if Schedule::from_str(&normalized).is_err() {
            bail!("Job '{slug}' has invalid cron expression '{expr}'");
        }
    }

    let queue = get_string(config, "queue").unwrap_or_else(|| "default".to_string());
    let retries = config
        .get::<Option<u32>>("retries")
        .ok()
        .flatten()
        .unwrap_or(0);
    let timeout = config
        .get::<Option<u64>>("timeout")
        .ok()
        .flatten()
        .unwrap_or(60);
    let concurrency = config
        .get::<Option<u32>>("concurrency")
        .ok()
        .flatten()
        .unwrap_or(1);
    let skip_if_running = get_bool(config, "skip_if_running", true)?;
    let access = get_string(config, "access");

    let labels = if let Ok(labels_tbl) = get_table(config, "labels") {
        JobLabels {
            singular: get_string(&labels_tbl, "singular"),
        }
    } else {
        JobLabels::default()
    };

    let mut builder = JobDefinitionBuilder::new(slug, handler)
        .queue(queue)
        .retries(retries)
        .timeout(timeout)
        .concurrency(concurrency)
        .skip_if_running(skip_if_running)
        .labels(labels);

    if let Some(s) = schedule {
        builder = builder.schedule(s);
    }

    if let Some(a) = access {
        builder = builder.access(a);
    }

    Ok(builder.build())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mlua::Lua;

    #[test]
    fn test_parse_job_definition_minimal() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("handler", "jobs.my_job.run").unwrap();

        let job = parse_job_definition("my-job", &tbl).unwrap();
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
        let tbl = lua.create_table().unwrap();
        tbl.set("handler", "jobs.sync.run").unwrap();
        tbl.set("schedule", "*/5 * * * *").unwrap();
        tbl.set("queue", "sync").unwrap();
        tbl.set("retries", 3u32).unwrap();
        tbl.set("timeout", 300u64).unwrap();
        tbl.set("concurrency", 2u32).unwrap();
        tbl.set("skip_if_running", false).unwrap();
        tbl.set("access", "access.admin_only").unwrap();

        let labels_tbl = lua.create_table().unwrap();
        labels_tbl.set("singular", "Sync Job").unwrap();
        tbl.set("labels", labels_tbl).unwrap();

        let job = parse_job_definition("sync", &tbl).unwrap();
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
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let result = parse_job_definition("bad-job", &tbl);
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
        let tbl = lua.create_table().unwrap();
        tbl.set("handler", "jobs.bad.run").unwrap();
        tbl.set("schedule", "not a cron").unwrap();
        let result = parse_job_definition("bad-job", &tbl);
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
        let tbl = lua.create_table().unwrap();
        tbl.set("handler", "jobs.hourly.run").unwrap();
        tbl.set("schedule", "0 0 * * * * *").unwrap();
        let job = parse_job_definition("hourly", &tbl).unwrap();
        assert_eq!(job.schedule.as_deref(), Some("0 0 * * * * *"));
    }
}
