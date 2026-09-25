//! `images` command — manage queued image-format conversions.
//!
//! As of alpha.9, image conversion lives in the unified job queue
//! (`_crap_jobs`) as `_system_image_convert` system jobs. This command
//! is a thin operator wrapper around that table, filtered to the
//! image-convert slug.

use anyhow::{Context as _, Result, anyhow, bail};
use std::path::Path;

use super::ImagesAction;
use crate::{
    cli::{self, Table},
    commands::helpers::{Project, open_project},
    config::parse_duration_string,
    core::{JobStatus, upload::SYSTEM_IMAGE_CONVERT_JOB},
    db::{BoxedConnection, DbPool, query::jobs as job_query},
};

/// Handle the `images` subcommand — dispatches to the appropriate action handler.
///
/// # Errors
///
/// Returns an error if config loading, pool creation, schema migration,
/// or the dispatched action fails.
#[cfg(not(tarpaulin_include))]
pub fn run(config_dir: &Path, action: ImagesAction) -> Result<()> {
    // `open_project` runs `sync_all` which ensures the `_crap_jobs`
    // schema (priority + unique_key columns, indexes, legacy
    // image-queue drain) is up to date before any read. Operators
    // upgrading from alpha.8 who run `images list` before `serve`
    // would otherwise hit a "no such column: priority" error.
    let Project {
        lock: _instance_lock,
        config: _cfg,
        registry: _registry,
        pool,
    } = open_project(config_dir)?;

    // Reads on the read pool; the writes (retry, purge) on the write pool,
    // like every other write.
    match action {
        ImagesAction::List { status, limit } => {
            list_entries(&read_conn(&pool)?, status.as_deref(), limit)
        }
        ImagesAction::Stats => show_stats(&read_conn(&pool)?),
        ImagesAction::Retry {
            id,
            all,
            confirm,
            priority,
        } => retry_entries(&write_conn(&pool)?, id, all, confirm, priority),
        ImagesAction::Purge { older_than } => purge_entries(&write_conn(&pool)?, &older_than),
    }
}

/// A connection for the read-only actions.
fn read_conn(pool: &DbPool) -> Result<BoxedConnection> {
    pool.get().context("Failed to get DB connection")
}

/// A connection for the actions that write.
fn write_conn(pool: &DbPool) -> Result<BoxedConnection> {
    pool.write().context("Failed to get a write connection")
}

/// List image-convert job runs with optional status filter.
fn list_entries(conn: &BoxedConnection, status: Option<&str>, limit: i64) -> Result<()> {
    // An unknown status filter matches nothing — mirror the empty result the
    // raw-string filter produced before the query layer took a typed status.
    let status = match status.map(JobStatus::from_name) {
        Some(None) => {
            cli::info("No image-convert jobs found.");
            return Ok(());
        }
        other => other.flatten(),
    };

    let entries = job_query::list_job_runs(conn, Some(SYSTEM_IMAGE_CONVERT_JOB), status, limit, 0)?;

    if entries.is_empty() {
        cli::info("No image-convert jobs found.");
        return Ok(());
    }

    let mut table = Table::new(vec![
        "ID",
        "Collection",
        "Document",
        "Format",
        "Prio",
        "Created",
        "Status",
    ]);

    for e in &entries {
        // Decode the payload so we can show the collection / document /
        // format columns the user expects from the legacy `images list`.
        let payload: serde_json::Value =
            serde_json::from_str(&e.data).unwrap_or(serde_json::Value::Null);
        let collection = payload
            .get("collection")
            .and_then(|v| v.as_str())
            .unwrap_or("-");
        let document_id = payload
            .get("document_id")
            .and_then(|v| v.as_str())
            .unwrap_or("-");
        let format = payload
            .get("format")
            .and_then(|v| v.as_str())
            .unwrap_or("-");

        let created = e.created_at.as_deref().unwrap_or("-");
        let priority = e.priority.to_string();

        let status_str = match &e.status {
            crate::core::job::JobStatus::Failed => {
                format!("failed: {}", e.error.as_deref().unwrap_or("unknown"))
            }
            other => other.as_str().to_string(),
        };

        let id_display: String = e.id.chars().take(22).collect();
        let doc_display: String = document_id.chars().take(10).collect();

        table.row(vec![
            &id_display,
            collection,
            &doc_display,
            format,
            &priority,
            created,
            &status_str,
        ]);
    }

    table.print();
    table.footer(&format!("{} entry/entries", entries.len()));

    Ok(())
}

/// Show queue statistics by status.
fn show_stats(conn: &BoxedConnection) -> Result<()> {
    let pending = job_query::count_job_runs(
        conn,
        Some(SYSTEM_IMAGE_CONVERT_JOB),
        Some(JobStatus::Pending),
    )?;
    let running = job_query::count_job_runs(
        conn,
        Some(SYSTEM_IMAGE_CONVERT_JOB),
        Some(JobStatus::Running),
    )?;
    let completed = job_query::count_job_runs(
        conn,
        Some(SYSTEM_IMAGE_CONVERT_JOB),
        Some(JobStatus::Completed),
    )?;
    let failed = job_query::count_job_runs(
        conn,
        Some(SYSTEM_IMAGE_CONVERT_JOB),
        Some(JobStatus::Failed),
    )?;

    cli::header("Image processing queue");
    cli::kv("Pending", &pending.to_string());
    cli::kv("Running", &running.to_string());
    cli::kv("Completed", &completed.to_string());
    cli::kv("Failed", &failed.to_string());
    cli::kv(
        "Total",
        &(pending + running + completed + failed).to_string(),
    );

    Ok(())
}

/// Retry failed image-convert jobs — either a single entry by ID or all failed.
/// `priority` is set on the retried rows so an operator can urgent-bump a
/// reset (default `0` keeps the original FIFO behaviour).
fn retry_entries(
    conn: &BoxedConnection,
    id: Option<String>,
    all: bool,
    confirm: bool,
    priority: i32,
) -> Result<()> {
    if all {
        if !confirm {
            bail!("Use -y to confirm retrying all failed entries");
        }

        let count = job_query::retry_failed_jobs(conn, SYSTEM_IMAGE_CONVERT_JOB, None, priority)?;

        cli::success(&format!("Reset {count} failed entry/entries to pending"));

        return Ok(());
    }

    let Some(entry_id) = id else {
        bail!("Specify --id <id> or --all -y");
    };

    let count =
        job_query::retry_failed_jobs(conn, SYSTEM_IMAGE_CONVERT_JOB, Some(&entry_id), priority)?;

    if count == 0 {
        bail!("Entry '{entry_id}' not found or not in 'failed' status");
    }

    cli::success(&format!("Reset entry {entry_id} to pending"));

    Ok(())
}

/// Purge finished image-convert jobs whose run ended longer ago than the
/// specified duration.
fn purge_entries(conn: &BoxedConnection, older_than: &str) -> Result<()> {
    let secs = parse_duration_string(older_than).ok_or_else(|| {
        anyhow!(
            "Invalid duration '{older_than}'. Use format like '7d' (days), '24h' (hours), '30m' (minutes), '60s' (seconds)"
        )
    })?;

    let deleted = job_query::purge_old_jobs_for_slug(conn, SYSTEM_IMAGE_CONVERT_JOB, secs)?;

    cli::success(&format!("Purged {deleted} old queue entry/entries"));

    Ok(())
}
