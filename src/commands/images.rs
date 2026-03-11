//! `images` command — manage the image processing queue.

use anyhow::{Context as _, Result};

/// Handle the `images` subcommand.
// Excluded from coverage: requires full Lua + DB setup for each variant.
#[cfg(not(tarpaulin_include))]
pub fn run(action: super::ImagesAction) -> Result<()> {
    match action {
        super::ImagesAction::List {
            config,
            status,
            limit,
        } => {
            let config_dir = config.canonicalize().unwrap_or(config);
            let cfg = crate::config::CrapConfig::load(&config_dir)?;
            let pool = crate::db::pool::create_pool(&config_dir, &cfg)?;

            let conn = pool.get().context("Failed to get DB connection")?;
            let status_filter = status.as_deref();
            let entries =
                crate::db::query::images::list_image_entries(&conn, status_filter, limit)?;

            if entries.is_empty() {
                println!("No queue entries found.");
                return Ok(());
            }

            println!(
                "{:<24} {:<12} {:<12} {:<8} {:<20} Status",
                "ID", "Collection", "Document", "Format", "Created"
            );
            println!("{}", "-".repeat(90));

            for e in &entries {
                let created = e.created_at.as_deref().unwrap_or("-");
                let status_str = if e.status == "failed" {
                    format!("failed: {}", e.error.as_deref().unwrap_or("unknown"))
                } else {
                    e.status.clone()
                };
                println!(
                    "{:<24} {:<12} {:<12} {:<8} {:<20} {}",
                    &e.id[..e.id.len().min(22)],
                    e.collection,
                    &e.document_id[..e.document_id.len().min(10)],
                    e.format,
                    created,
                    status_str
                );
            }

            println!("\n{} entry/entries", entries.len());
            Ok(())
        }
        super::ImagesAction::Stats { config } => {
            let config_dir = config.canonicalize().unwrap_or(config);
            let cfg = crate::config::CrapConfig::load(&config_dir)?;
            let pool = crate::db::pool::create_pool(&config_dir, &cfg)?;

            let conn = pool.get().context("Failed to get DB connection")?;

            let pending =
                crate::db::query::images::count_image_entries_by_status(&conn, "pending")?;
            let processing =
                crate::db::query::images::count_image_entries_by_status(&conn, "processing")?;
            let completed =
                crate::db::query::images::count_image_entries_by_status(&conn, "completed")?;
            let failed = crate::db::query::images::count_image_entries_by_status(&conn, "failed")?;

            println!("Image processing queue:");
            println!("  Pending:    {}", pending);
            println!("  Processing: {}", processing);
            println!("  Completed:  {}", completed);
            println!("  Failed:     {}", failed);
            println!(
                "  Total:      {}",
                pending + processing + completed + failed
            );

            Ok(())
        }
        super::ImagesAction::Retry {
            config,
            id,
            all,
            confirm,
        } => {
            let config_dir = config.canonicalize().unwrap_or(config);
            let cfg = crate::config::CrapConfig::load(&config_dir)?;
            let pool = crate::db::pool::create_pool(&config_dir, &cfg)?;

            let conn = pool.get().context("Failed to get DB connection")?;

            if all {
                if !confirm {
                    anyhow::bail!("Use -y to confirm retrying all failed entries");
                }
                let count = crate::db::query::images::retry_all_failed_images(&conn)?;
                println!("Reset {} failed entry/entries to pending", count);
            } else if let Some(entry_id) = id {
                let found = crate::db::query::images::retry_image_entry(&conn, &entry_id)?;
                if found {
                    println!("Reset entry {} to pending", entry_id);
                } else {
                    anyhow::bail!("Entry '{}' not found or not in 'failed' status", entry_id);
                }
            } else {
                anyhow::bail!("Specify --id <id> or --all -y");
            }

            Ok(())
        }
        super::ImagesAction::Purge { config, older_than } => {
            let config_dir = config.canonicalize().unwrap_or(config);
            let cfg = crate::config::CrapConfig::load(&config_dir)?;
            let pool = crate::db::pool::create_pool(&config_dir, &cfg)?;

            let secs = crate::config::parse_duration_string(&older_than)
                .ok_or_else(|| anyhow::anyhow!(
                    "Invalid duration '{}'. Use format like '7d' (days), '24h' (hours), '30m' (minutes), '60s' (seconds)",
                    older_than
                ))?;

            let conn = pool.get().context("Failed to get DB connection")?;
            let deleted = crate::db::query::images::purge_old_image_entries(&conn, secs)?;
            println!("Purged {} old queue entry/entries", deleted);

            Ok(())
        }
    }
}
