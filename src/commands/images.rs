//! `images` command — manage the image processing queue.

use anyhow::{Context as _, Result, anyhow, bail};
use std::path::Path;

use super::ImagesAction;
use crate::{
    cli::{self, Table},
    config::{CrapConfig, parse_duration_string},
    db::{pool, query},
};

/// Handle the `images` subcommand.
// Excluded from coverage: requires full Lua + DB setup for each variant.
#[cfg(not(tarpaulin_include))]
pub fn run(config_dir: &Path, action: super::ImagesAction) -> Result<()> {
    match action {
        ImagesAction::List { status, limit } => {
            let config_dir = config_dir
                .canonicalize()
                .unwrap_or_else(|_| config_dir.to_path_buf());
            let cfg = CrapConfig::load(&config_dir)?;
            let pool = pool::create_pool(&config_dir, &cfg)?;

            let conn = pool.get().context("Failed to get DB connection")?;
            let status_filter = status.as_deref();
            let entries = query::images::list_image_entries(&conn, status_filter, limit)?;

            if entries.is_empty() {
                cli::info("No queue entries found.");

                return Ok(());
            }

            let mut table = Table::new(vec![
                "ID",
                "Collection",
                "Document",
                "Format",
                "Created",
                "Status",
            ]);

            for e in &entries {
                let created = e.created_at.as_deref().unwrap_or("-");
                let status_str = if e.status == "failed" {
                    format!("failed: {}", e.error.as_deref().unwrap_or("unknown"))
                } else {
                    e.status.clone()
                };
                let id_display: String = e.id.chars().take(22).collect();
                let doc_display: String = e.document_id.chars().take(10).collect();

                table.row(vec![
                    &id_display,
                    &e.collection,
                    &doc_display,
                    &e.format,
                    created,
                    &status_str,
                ]);
            }

            table.print();
            table.footer(&format!("{} entry/entries", entries.len()));

            Ok(())
        }
        ImagesAction::Stats => {
            let config_dir = config_dir
                .canonicalize()
                .unwrap_or_else(|_| config_dir.to_path_buf());
            let cfg = CrapConfig::load(&config_dir)?;
            let pool = pool::create_pool(&config_dir, &cfg)?;

            let conn = pool.get().context("Failed to get DB connection")?;

            let pending = query::images::count_image_entries_by_status(&conn, "pending")?;
            let processing = query::images::count_image_entries_by_status(&conn, "processing")?;
            let completed = query::images::count_image_entries_by_status(&conn, "completed")?;
            let failed = query::images::count_image_entries_by_status(&conn, "failed")?;

            cli::header("Image processing queue");
            cli::kv("Pending", &pending.to_string());
            cli::kv("Processing", &processing.to_string());
            cli::kv("Completed", &completed.to_string());
            cli::kv("Failed", &failed.to_string());
            cli::kv(
                "Total",
                &(pending + processing + completed + failed).to_string(),
            );

            Ok(())
        }
        ImagesAction::Retry { id, all, confirm } => {
            let config_dir = config_dir
                .canonicalize()
                .unwrap_or_else(|_| config_dir.to_path_buf());
            let cfg = CrapConfig::load(&config_dir)?;
            let pool = pool::create_pool(&config_dir, &cfg)?;

            let conn = pool.get().context("Failed to get DB connection")?;

            if all {
                if !confirm {
                    bail!("Use -y to confirm retrying all failed entries");
                }

                let count = query::images::retry_all_failed_images(&conn)?;

                cli::success(&format!("Reset {} failed entry/entries to pending", count));
            } else if let Some(entry_id) = id {
                let found = query::images::retry_image_entry(&conn, &entry_id)?;

                if found {
                    cli::success(&format!("Reset entry {} to pending", entry_id));
                } else {
                    bail!("Entry '{}' not found or not in 'failed' status", entry_id);
                }
            } else {
                bail!("Specify --id <id> or --all -y");
            }

            Ok(())
        }
        ImagesAction::Purge { older_than } => {
            let config_dir = config_dir
                .canonicalize()
                .unwrap_or_else(|_| config_dir.to_path_buf());
            let cfg = CrapConfig::load(&config_dir)?;
            let pool = pool::create_pool(&config_dir, &cfg)?;

            let secs = parse_duration_string(&older_than)
                .ok_or_else(|| anyhow!(
                    "Invalid duration '{}'. Use format like '7d' (days), '24h' (hours), '30m' (minutes), '60s' (seconds)",
                    older_than
                ))?;

            let conn = pool.get().context("Failed to get DB connection")?;
            let deleted = query::images::purge_old_image_entries(&conn, secs)?;

            cli::success(&format!("Purged {} old queue entry/entries", deleted));

            Ok(())
        }
    }
}
