//! `make job` — generate job Lua files.

use std::{fs, path::Path};

use anyhow::{Context as _, Result, bail};
use serde_json::json;

use crate::cli;
use crate::scaffold::{render::render, to_title_case, validate_slug};
use crate::typegen::to_pascal_case;

/// Options for `make_job()`.
pub struct MakeJobOptions<'a> {
    pub config_dir: &'a Path,
    pub slug: &'a str,
    pub schedule: Option<&'a str>,
    pub queue: Option<&'a str>,
    pub retries: Option<u32>,
    pub timeout: Option<u64>,
    pub force: bool,
}

/// Scaffold a job Lua file in `jobs/<slug>.lua`.
///
/// Generates a module with a `run` handler, followed by `crap.jobs.define()`.
pub fn make_job(opts: &MakeJobOptions) -> Result<()> {
    validate_slug(opts.slug)?;

    let jobs_dir = opts.config_dir.join("jobs");
    fs::create_dir_all(&jobs_dir).context("Failed to create jobs/ directory")?;

    let file_path = jobs_dir.join(format!("{}.lua", opts.slug));

    if file_path.exists() && !opts.force {
        bail!(
            "File '{}' already exists — use --force to overwrite",
            file_path.display()
        );
    }

    let lua = render_job_lua(opts)?;

    fs::write(&file_path, &lua)
        .with_context(|| format!("Failed to write {}", file_path.display()))?;

    let handler_ref = format!("jobs.{}.run", opts.slug);
    cli::success(&format!("Created {}", file_path.display()));
    cli::kv("Handler ref", &handler_ref);
    print_hint(opts);

    Ok(())
}

/// Render the job Lua file via Handlebars.
fn render_job_lua(opts: &MakeJobOptions) -> Result<String> {
    let label = to_title_case(opts.slug);
    let pascal = to_pascal_case(opts.slug);

    // Filter out default values so {{#if}} in the template skips them
    let queue = opts.queue.filter(|q| *q != "default");
    let retries = opts.retries.filter(|&r| r > 0);
    let timeout = opts.timeout.filter(|&t| t != 60);

    render(
        "job",
        &json!({
            "label": label,
            "slug": opts.slug,
            "pascal": pascal,
            "schedule": opts.schedule,
            "queue": queue,
            "retries": retries,
            "timeout": timeout,
        }),
    )
}

/// Print the appropriate hint after creating a job.
fn print_hint(opts: &MakeJobOptions) {
    if opts.schedule.is_some() {
        cli::hint("This job has a cron schedule and will run automatically.");
        return;
    }

    cli::hint(&format!(
        "Queue from hooks:\n  crap.jobs.queue(\"{}\", {{ key = \"value\" }})\n\nOr trigger from CLI:\n  crap-cms jobs trigger {}",
        opts.slug, opts.slug
    ));
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use super::*;

    /// Build options with common defaults for testing.
    fn opts<'a>(
        config_dir: &'a Path,
        slug: &'a str,
        schedule: Option<&'a str>,
        queue: Option<&'a str>,
        retries: Option<u32>,
        timeout: Option<u64>,
        force: bool,
    ) -> MakeJobOptions<'a> {
        MakeJobOptions {
            config_dir,
            slug,
            schedule,
            queue,
            retries,
            timeout,
            force,
        }
    }

    #[test]
    fn make_default() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_job(&opts(tmp.path(), "cleanup", None, None, None, None, false)).unwrap();

        let content = fs::read_to_string(tmp.path().join("jobs/cleanup.lua")).unwrap();
        assert!(content.contains("local M = {}"));
        assert!(content.contains("crap.JobHandlerContext"));
        assert!(content.contains("function M.run(context)"));
        assert!(content.contains("crap.jobs.define(\"cleanup\""));
        assert!(content.contains("handler = \"jobs.cleanup.run\""));
        assert!(content.contains("return M"));
    }

    #[test]
    fn data_class_hint() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_job(&opts(
            tmp.path(),
            "process_inquiry",
            None,
            None,
            None,
            None,
            false,
        ))
        .unwrap();
        let content = fs::read_to_string(tmp.path().join("jobs/process_inquiry.lua")).unwrap();
        assert!(content.contains("@class ProcessInquiryData"));
        assert!(content.contains("@type ProcessInquiryData"));
    }

    #[test]
    fn with_schedule() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_job(&opts(
            tmp.path(),
            "nightly",
            Some("0 3 * * *"),
            None,
            None,
            None,
            false,
        ))
        .unwrap();
        let content = fs::read_to_string(tmp.path().join("jobs/nightly.lua")).unwrap();
        assert!(content.contains("schedule = \"0 3 * * *\""));
    }

    #[test]
    fn with_queue() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_job(&opts(
            tmp.path(),
            "email_send",
            None,
            Some("email"),
            None,
            None,
            false,
        ))
        .unwrap();
        let content = fs::read_to_string(tmp.path().join("jobs/email_send.lua")).unwrap();
        assert!(content.contains("queue = \"email\""));
    }

    #[test]
    fn with_retries_and_timeout() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_job(&opts(
            tmp.path(),
            "import",
            None,
            None,
            Some(3),
            Some(300),
            false,
        ))
        .unwrap();
        let content = fs::read_to_string(tmp.path().join("jobs/import.lua")).unwrap();
        assert!(content.contains("retries = 3"));
        assert!(content.contains("timeout = 300"));
    }

    #[test]
    fn refuses_overwrite() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_job(&opts(tmp.path(), "cleanup", None, None, None, None, false)).unwrap();
        assert!(
            make_job(&opts(tmp.path(), "cleanup", None, None, None, None, false))
                .unwrap_err()
                .to_string()
                .contains("--force")
        );
    }

    #[test]
    fn force_overwrite() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_job(&opts(tmp.path(), "cleanup", None, None, None, None, false)).unwrap();
        assert!(make_job(&opts(tmp.path(), "cleanup", None, None, None, None, true)).is_ok());
    }

    #[test]
    fn invalid_slug() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert!(make_job(&opts(tmp.path(), "Bad Slug", None, None, None, None, false)).is_err());
    }

    #[test]
    fn ordering() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_job(&opts(tmp.path(), "sync", None, None, None, None, false)).unwrap();
        let content = fs::read_to_string(tmp.path().join("jobs/sync.lua")).unwrap();
        let module_pos = content.find("local M = {}").unwrap();
        let define_pos = content.find("crap.jobs.define").unwrap();
        let return_pos = content.rfind("return M").unwrap();
        assert!(module_pos < define_pos);
        assert!(define_pos < return_pos);
    }

    #[test]
    fn default_queue_not_emitted() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_job(&opts(
            tmp.path(),
            "cleanup",
            None,
            Some("default"),
            None,
            None,
            false,
        ))
        .unwrap();
        let content = fs::read_to_string(tmp.path().join("jobs/cleanup.lua")).unwrap();
        assert!(!content.contains("queue ="));
    }

    #[test]
    fn default_timeout_not_emitted() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_job(&opts(
            tmp.path(),
            "cleanup",
            None,
            None,
            None,
            Some(60),
            false,
        ))
        .unwrap();
        let content = fs::read_to_string(tmp.path().join("jobs/cleanup.lua")).unwrap();
        assert!(!content.contains("timeout ="));
    }
}
