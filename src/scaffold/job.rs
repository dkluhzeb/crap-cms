//! `make job` command — generate job Lua files.

use anyhow::{Context as _, Result};
use std::fs;
use std::path::Path;

use crate::typegen::to_pascal_case;

/// Scaffold a job Lua file in `jobs/<slug>.lua`.
///
/// Generates a module with a `run` handler, followed by `crap.jobs.define()`.
pub fn make_job(
    config_dir: &Path,
    slug: &str,
    schedule: Option<&str>,
    queue: Option<&str>,
    retries: Option<u32>,
    timeout: Option<u64>,
    force: bool,
) -> Result<()> {
    super::validate_slug(slug)?;

    let jobs_dir = config_dir.join("jobs");
    fs::create_dir_all(&jobs_dir).context("Failed to create jobs/ directory")?;

    let file_path = jobs_dir.join(format!("{}.lua", slug));
    if file_path.exists() && !force {
        anyhow::bail!(
            "File '{}' already exists — use --force to overwrite",
            file_path.display()
        );
    }

    let label = super::to_title_case(slug);
    let handler_ref = format!("jobs.{}.run", slug);

    // Build optional config lines
    let mut config_lines = Vec::new();
    config_lines.push(format!("    handler = \"{}\",", handler_ref));
    if let Some(sched) = schedule {
        config_lines.push(format!("    schedule = \"{}\",", sched));
    }
    if let Some(q) = queue {
        if q != "default" {
            config_lines.push(format!("    queue = \"{}\",", q));
        }
    }
    if let Some(r) = retries {
        if r > 0 {
            config_lines.push(format!("    retries = {},", r));
        }
    }
    if let Some(t) = timeout {
        if t != 60 {
            config_lines.push(format!("    timeout = {},", t));
        }
    }
    config_lines.push(format!("    labels = {{ singular = \"{}\" }},", label));

    let config_body = config_lines.join("\n");
    let pascal = to_pascal_case(slug);

    let lua = format!(
        r#"--- {label} job handler.
local M = {{}}

-- Type the data shape passed via crap.jobs.queue():
-- ---@class {pascal}Data
-- ---@field my_field string

---@param context crap.JobHandlerContext
---@return table?
function M.run(context)
    -- context.data = input data from queue() or {{}} for cron
    -- context.job  = {{ slug, attempt, max_attempts }}
    -- Full CRUD access: crap.collections.find(), .create(), etc.

    -- ---@type {pascal}Data
    -- local data = context.data

    -- TODO: implement
    return nil
end

crap.jobs.define("{slug}", {{
{config_body}
}})

return M
"#,
        label = label,
        slug = slug,
        pascal = pascal,
        config_body = config_body,
    );

    fs::write(&file_path, &lua)
        .with_context(|| format!("Failed to write {}", file_path.display()))?;

    println!("Created {}", file_path.display());
    println!();
    println!("Handler ref: {}", handler_ref);

    if schedule.is_some() {
        println!();
        println!("This job has a cron schedule and will run automatically.");
    } else {
        println!();
        println!("Queue from hooks:");
        println!("  crap.jobs.queue(\"{}\", {{ key = \"value\" }})", slug);
        println!();
        println!("Or trigger from CLI:");
        println!("  crap-cms jobs trigger <config> {}", slug);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_make_job_default() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_job(tmp.path(), "cleanup", None, None, None, None, false).unwrap();

        let content = fs::read_to_string(tmp.path().join("jobs/cleanup.lua")).unwrap();
        assert!(content.contains("local M = {}"));
        assert!(content.contains("crap.JobHandlerContext"));
        assert!(content.contains("function M.run(context)"));
        assert!(content.contains("crap.jobs.define(\"cleanup\""));
        assert!(content.contains("handler = \"jobs.cleanup.run\""));
        assert!(content.contains("return M"));
    }

    #[test]
    fn test_make_job_data_class_hint() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_job(tmp.path(), "process_inquiry", None, None, None, None, false).unwrap();

        let content = fs::read_to_string(tmp.path().join("jobs/process_inquiry.lua")).unwrap();
        assert!(
            content.contains("@class ProcessInquiryData"),
            "should hint PascalCase data class, got:\n{content}"
        );
        assert!(
            content.contains("@type ProcessInquiryData"),
            "should hint data cast"
        );
    }

    #[test]
    fn test_make_job_with_schedule() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_job(
            tmp.path(),
            "nightly",
            Some("0 3 * * *"),
            None,
            None,
            None,
            false,
        )
        .unwrap();

        let content = fs::read_to_string(tmp.path().join("jobs/nightly.lua")).unwrap();
        assert!(content.contains("schedule = \"0 3 * * *\""));
    }

    #[test]
    fn test_make_job_with_queue() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_job(
            tmp.path(),
            "email_send",
            None,
            Some("email"),
            None,
            None,
            false,
        )
        .unwrap();

        let content = fs::read_to_string(tmp.path().join("jobs/email_send.lua")).unwrap();
        assert!(content.contains("queue = \"email\""));
    }

    #[test]
    fn test_make_job_with_options() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_job(tmp.path(), "import", None, None, Some(3), Some(300), false).unwrap();

        let content = fs::read_to_string(tmp.path().join("jobs/import.lua")).unwrap();
        assert!(content.contains("retries = 3"));
        assert!(content.contains("timeout = 300"));
    }

    #[test]
    fn test_make_job_refuses_overwrite() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_job(tmp.path(), "cleanup", None, None, None, None, false).unwrap();
        let result = make_job(tmp.path(), "cleanup", None, None, None, None, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("--force"));
    }

    #[test]
    fn test_make_job_force_overwrite() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_job(tmp.path(), "cleanup", None, None, None, None, false).unwrap();
        assert!(make_job(tmp.path(), "cleanup", None, None, None, None, true).is_ok());
    }

    #[test]
    fn test_make_job_invalid_slug() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let result = make_job(tmp.path(), "Bad Slug", None, None, None, None, false);
        assert!(result.is_err());
    }

    #[test]
    fn test_make_job_ordering() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_job(tmp.path(), "sync", None, None, None, None, false).unwrap();

        let content = fs::read_to_string(tmp.path().join("jobs/sync.lua")).unwrap();
        let module_pos = content
            .find("local M = {}")
            .expect("local M = {} not found");
        let define_pos = content
            .find("crap.jobs.define")
            .expect("crap.jobs.define not found");
        let return_pos = content.rfind("return M").expect("return M not found");
        assert!(module_pos < define_pos, "module should come before define");
        assert!(
            define_pos < return_pos,
            "define should come before return M"
        );
    }

    #[test]
    fn test_make_job_default_queue_not_emitted() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_job(
            tmp.path(),
            "cleanup",
            None,
            Some("default"),
            None,
            None,
            false,
        )
        .unwrap();

        let content = fs::read_to_string(tmp.path().join("jobs/cleanup.lua")).unwrap();
        assert!(
            !content.contains("queue ="),
            "default queue should not be emitted"
        );
    }

    #[test]
    fn test_make_job_default_timeout_not_emitted() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_job(tmp.path(), "cleanup", None, None, None, Some(60), false).unwrap();

        let content = fs::read_to_string(tmp.path().join("jobs/cleanup.lua")).unwrap();
        assert!(
            !content.contains("timeout ="),
            "default timeout (60) should not be emitted"
        );
    }
}
