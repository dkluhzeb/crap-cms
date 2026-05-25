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
    commands::helpers::init_stack,
    config::parse_duration_string,
    core::upload::SYSTEM_IMAGE_CONVERT_JOB,
    db::{BoxedConnection, DbConnection, DbValue, query::jobs as job_query},
};

/// Handle the `images` subcommand — dispatches to the appropriate action handler.
///
/// # Errors
///
/// Returns an error if config loading, pool creation, schema migration,
/// or the dispatched action fails.
#[cfg(not(tarpaulin_include))]
pub fn run(config_dir: &Path, action: ImagesAction) -> Result<()> {
    // `init_stack` runs `sync_all` which ensures the `_crap_jobs`
    // schema (priority + unique_key columns, indexes, legacy
    // image-queue drain) is up to date before any read. Operators
    // upgrading from alpha.8 who run `images list` before `serve`
    // would otherwise hit a "no such column: priority" error.
    let (_cfg, _registry, pool) = init_stack(config_dir)?;
    let conn = pool.get().context("Failed to get DB connection")?;

    match action {
        ImagesAction::List { status, limit } => list_entries(&conn, status.as_deref(), limit),
        ImagesAction::Stats => show_stats(&conn),
        ImagesAction::Retry {
            id,
            all,
            confirm,
            priority,
        } => retry_entries(&conn, id, all, confirm, priority),
        ImagesAction::Purge { older_than } => purge_entries(&conn, &older_than),
    }
}

/// List image-convert job runs with optional status filter.
fn list_entries(conn: &BoxedConnection, status: Option<&str>, limit: i64) -> Result<()> {
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
    let pending = job_query::count_job_runs(conn, Some(SYSTEM_IMAGE_CONVERT_JOB), Some("pending"))?;
    let running = job_query::count_job_runs(conn, Some(SYSTEM_IMAGE_CONVERT_JOB), Some("running"))?;
    let completed =
        job_query::count_job_runs(conn, Some(SYSTEM_IMAGE_CONVERT_JOB), Some("completed"))?;
    let failed = job_query::count_job_runs(conn, Some(SYSTEM_IMAGE_CONVERT_JOB), Some("failed"))?;

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

        let count = conn.execute(
            &format!(
                "UPDATE _crap_jobs SET status = 'pending', error = NULL, attempt = 0, \
                                 completed_at = NULL, started_at = NULL, retry_after = NULL, \
                                 priority = {} \
                 WHERE slug = {} AND status = 'failed'",
                conn.placeholder(1),
                conn.placeholder(2)
            ),
            &[
                DbValue::Integer(i64::from(priority)),
                DbValue::Text(SYSTEM_IMAGE_CONVERT_JOB.to_string()),
            ],
        )?;

        cli::success(&format!("Reset {count} failed entry/entries to pending"));
    } else if let Some(entry_id) = id {
        let count = conn.execute(
            &format!(
                "UPDATE _crap_jobs SET status = 'pending', error = NULL, attempt = 0, \
                                 completed_at = NULL, started_at = NULL, retry_after = NULL, \
                                 priority = {} \
                 WHERE id = {} AND slug = {} AND status = 'failed'",
                conn.placeholder(1),
                conn.placeholder(2),
                conn.placeholder(3)
            ),
            &[
                DbValue::Integer(i64::from(priority)),
                DbValue::Text(entry_id.clone()),
                DbValue::Text(SYSTEM_IMAGE_CONVERT_JOB.to_string()),
            ],
        )?;

        if count > 0 {
            cli::success(&format!("Reset entry {entry_id} to pending"));
        } else {
            bail!("Entry '{entry_id}' not found or not in 'failed' status");
        }
    } else {
        bail!("Specify --id <id> or --all -y");
    }

    Ok(())
}

/// Purge old completed/failed image-convert jobs older than the
/// specified duration.
fn purge_entries(conn: &BoxedConnection, older_than: &str) -> Result<()> {
    let secs = parse_duration_string(older_than).ok_or_else(|| {
        anyhow!(
            "Invalid duration '{older_than}'. Use format like '7d' (days), '24h' (hours), '30m' (minutes), '60s' (seconds)"
        )
    })?;

    let secs_i64 = i64::try_from(secs).context("Duration overflows i64 seconds")?;

    let (offset_sql, offset_param) = conn.date_offset_expr(-secs_i64, 2);
    let deleted = conn.execute(
        &format!(
            "DELETE FROM _crap_jobs \
             WHERE slug = {} AND status IN ('completed', 'failed') \
                AND created_at < {}",
            conn.placeholder(1),
            offset_sql,
        ),
        &[
            DbValue::Text(SYSTEM_IMAGE_CONVERT_JOB.to_string()),
            offset_param,
        ],
    )?;

    cli::success(&format!("Purged {deleted} old queue entry/entries"));

    Ok(())
}
