//! `trash` command — manage soft-deleted documents.

use std::path::Path;

use anyhow::{Context as _, Result, anyhow};

use super::TrashAction;
use crate::{
    cli::{self, Table},
    config::CrapConfig,
    core::{SharedRegistry, upload},
    db::{DbConnection, DbPool, DbValue, migrate, pool, query},
    hooks,
};

/// Initialize config, Lua, pool, and migrate. Used by all trash subcommands.
fn init_stack(config_dir: &Path) -> Result<(CrapConfig, SharedRegistry, DbPool)> {
    let config_dir = config_dir
        .canonicalize()
        .unwrap_or_else(|_| config_dir.to_path_buf());
    let cfg = CrapConfig::load(&config_dir)?;
    let registry = hooks::init_lua(&config_dir, &cfg)?;
    let pool = pool::create_pool(&config_dir, &cfg)?;
    migrate::sync_all(&pool, &registry, &cfg.locale)?;

    Ok((cfg, registry, pool))
}

/// Collect slugs of collections that have `soft_delete = true`.
/// If `filter` is provided, only return that collection (validating it exists and supports soft delete).
fn resolve_collections(registry: &SharedRegistry, filter: Option<&str>) -> Result<Vec<String>> {
    let reg = registry
        .read()
        .map_err(|e| anyhow!("Registry lock poisoned: {}", e))?;

    if let Some(slug) = filter {
        let def = reg
            .collections
            .get(slug)
            .ok_or_else(|| anyhow!("Collection '{}' not found", slug))?;

        if !def.soft_delete {
            anyhow::bail!("Collection '{}' does not have soft_delete enabled", slug);
        }

        return Ok(vec![slug.to_string()]);
    }

    let mut slugs: Vec<String> = reg
        .collections
        .iter()
        .filter(|(_, def)| def.soft_delete)
        .map(|(slug, _)| slug.to_string())
        .collect();

    slugs.sort();

    Ok(slugs)
}

/// List trashed (soft-deleted) documents across collections.
fn run_list(
    registry: &SharedRegistry,
    pool: &DbPool,
    cfg: &CrapConfig,
    collection: Option<&str>,
) -> Result<()> {
    let slugs = resolve_collections(registry, collection)?;

    if slugs.is_empty() {
        cli::info("No collections with soft_delete enabled.");
        return Ok(());
    }

    let reg = registry
        .read()
        .map_err(|e| anyhow!("Registry lock poisoned: {}", e))?;
    let conn = pool.get().context("Failed to get DB connection")?;
    let locale_ctx = query::LocaleContext::from_locale_string(None, &cfg.locale);

    let mut table = Table::new(vec!["ID", "Title", "Collection", "Deleted At"]);
    let mut total = 0usize;

    for slug in &slugs {
        let def = match reg.collections.get(slug.as_str()) {
            Some(d) => d,
            None => continue,
        };

        let mut fq = query::FindQuery::new();
        fq.include_deleted = true;
        fq.filters = vec![query::FilterClause::Single(query::Filter {
            field: "_deleted_at".to_string(),
            op: query::FilterOp::Exists,
        })];

        let docs = query::find(&conn, slug, def, &fq, locale_ctx.as_ref())?;
        let title_field = def.title_field().unwrap_or("id");

        for doc in &docs {
            let id = doc.id.to_string();

            let title = doc
                .fields
                .get(title_field)
                .and_then(|v| v.as_str())
                .unwrap_or("-")
                .to_string();

            let deleted_at = doc
                .fields
                .get("_deleted_at")
                .and_then(|v| v.as_str())
                .unwrap_or("-")
                .to_string();

            table.row(vec![&id, &title, slug, &deleted_at]);
            total += 1;
        }
    }

    if total == 0 {
        cli::info("No trashed documents found.");
    } else {
        table.print();
        table.footer(&format!("{} trashed document(s)", total));
    }

    Ok(())
}

/// Parse a duration string like "30d", "7d", "24h" into seconds.
/// Returns `None` for "all" or invalid input.
fn parse_older_than(s: &str) -> Option<i64> {
    let s = s.trim();

    if s == "all" {
        return None;
    }

    if let Some(days) = s.strip_suffix('d') {
        days.parse::<i64>().ok().map(|d| d * 86400)
    } else if let Some(hours) = s.strip_suffix('h') {
        hours.parse::<i64>().ok().map(|h| h * 3600)
    } else if let Some(mins) = s.strip_suffix('m') {
        mins.parse::<i64>().ok().map(|m| m * 60)
    } else {
        s.parse::<i64>().ok()
    }
}

/// Purge (permanently delete) trashed documents, optionally filtered by age.
fn run_purge(
    registry: &SharedRegistry,
    pool: &DbPool,
    config_dir: &Path,
    collection: Option<&str>,
    older_than: &str,
    dry_run: bool,
) -> Result<()> {
    let slugs = resolve_collections(registry, collection)?;

    if slugs.is_empty() {
        cli::info("No collections with soft_delete enabled.");
        return Ok(());
    }

    let threshold_secs = if older_than == "all" {
        None
    } else {
        let secs = parse_older_than(older_than).ok_or_else(|| {
            anyhow!(
                "Invalid duration '{}'. Use format like '30d' (days), '24h' (hours), '30m' (minutes), '60s' (seconds), or 'all'",
                older_than
            )
        })?;
        Some(secs)
    };

    let reg = registry
        .read()
        .map_err(|e| anyhow!("Registry lock poisoned: {}", e))?;
    let mut conn = pool.get().context("Failed to get DB connection")?;

    let mut total = 0u64;

    for slug in &slugs {
        let def = match reg.collections.get(slug.as_str()) {
            Some(d) => d,
            None => continue,
        };

        let ids = find_purge_candidates(&conn as &dyn DbConnection, slug, threshold_secs)?;

        if ids.is_empty() {
            continue;
        }

        if dry_run {
            for id in &ids {
                cli::info(&format!("Would purge: {} / {}", slug, id));
            }
            total += ids.len() as u64;
            continue;
        }

        let tx = conn.transaction().context("Start transaction")?;

        for id in &ids {
            if def.is_upload_collection()
                && let Ok(Some(doc)) = query::find_by_id_unfiltered(&tx, slug, def, id, None)
            {
                upload::delete_upload_files(config_dir, &doc.fields);
            }

            query::fts::fts_delete(&tx, slug, id)?;
            query::delete(&tx, slug, id)?;
        }

        tx.commit().context("Commit purge")?;
        total += ids.len() as u64;

        // Re-acquire connection after commit (tx consumed it)
        conn = pool.get().context("Failed to get DB connection")?;
    }

    if dry_run {
        cli::info(&format!("{} document(s) would be purged.", total));
    } else {
        cli::success(&format!("Purged {} trashed document(s).", total));
    }

    Ok(())
}

/// Find IDs of soft-deleted documents eligible for purging in a collection.
fn find_purge_candidates(
    conn: &dyn DbConnection,
    slug: &str,
    threshold_secs: Option<i64>,
) -> Result<Vec<String>> {
    let (sql, params) = match threshold_secs {
        Some(secs) => {
            let (offset_sql, offset_param) = conn.date_offset_expr(secs, 1);
            (
                format!(
                    "SELECT id FROM \"{}\" WHERE _deleted_at IS NOT NULL \
                     AND _deleted_at < {}",
                    slug, offset_sql
                ),
                vec![offset_param],
            )
        }
        None => (
            format!("SELECT id FROM \"{}\" WHERE _deleted_at IS NOT NULL", slug),
            vec![],
        ),
    };

    let rows = conn.query_all(&sql, &params)?;
    let mut ids = Vec::new();

    for row in &rows {
        if let Some(DbValue::Text(id)) = row.get_value(0) {
            ids.push(id.clone());
        }
    }

    Ok(ids)
}

/// Restore a single soft-deleted document.
fn run_restore(registry: &SharedRegistry, pool: &DbPool, collection: &str, id: &str) -> Result<()> {
    let reg = registry
        .read()
        .map_err(|e| anyhow!("Registry lock poisoned: {}", e))?;

    let def = reg
        .collections
        .get(collection)
        .ok_or_else(|| anyhow!("Collection '{}' not found", collection))?;

    if !def.soft_delete {
        anyhow::bail!(
            "Collection '{}' does not have soft_delete enabled",
            collection
        );
    }

    let mut conn = pool.get().context("Failed to get DB connection")?;
    let tx = conn.transaction().context("Start transaction")?;

    let restored = query::restore(&tx, collection, id)?;

    if !restored {
        anyhow::bail!("Document '{}' not found or not in trash", id);
    }

    // Re-sync FTS index (FTS row was deleted on soft-delete)
    if tx.supports_fts()
        && let Ok(Some(doc)) = query::find_by_id_unfiltered(&tx, collection, def, id, None)
    {
        query::fts::fts_upsert(&tx, collection, &doc, Some(def))?;
    }

    tx.commit().context("Commit restore")?;

    cli::success(&format!("Restored document '{}' in '{}'.", id, collection));

    Ok(())
}

/// Permanently delete all trashed documents in a collection.
fn run_empty(
    registry: &SharedRegistry,
    pool: &DbPool,
    config_dir: &Path,
    collection: &str,
    confirm: bool,
) -> Result<()> {
    let reg = registry
        .read()
        .map_err(|e| anyhow!("Registry lock poisoned: {}", e))?;

    let def = reg
        .collections
        .get(collection)
        .ok_or_else(|| anyhow!("Collection '{}' not found", collection))?
        .clone();

    if !def.soft_delete {
        anyhow::bail!(
            "Collection '{}' does not have soft_delete enabled",
            collection
        );
    }

    // Drop the lock before DB operations
    drop(reg);

    let mut conn = pool.get().context("Failed to get DB connection")?;

    // Count trashed docs first
    let mut fq = query::FindQuery::new();
    fq.include_deleted = true;
    fq.filters = vec![query::FilterClause::Single(query::Filter {
        field: "_deleted_at".to_string(),
        op: query::FilterOp::Exists,
    })];

    let docs = query::find(&conn, collection, &def, &fq, None)?;
    let count = docs.len();

    if count == 0 {
        cli::info(&format!("No trashed documents in '{}'.", collection));
        return Ok(());
    }

    if !confirm {
        cli::warning(&format!(
            "This will permanently delete {} document(s) from '{}'.",
            count, collection
        ));
        cli::hint("Pass -y/--confirm to proceed.");
        return Ok(());
    }

    let tx = conn.transaction().context("Start transaction")?;

    for doc in &docs {
        if def.is_upload_collection() {
            upload::delete_upload_files(config_dir, &doc.fields);
        }

        query::fts::fts_delete(&tx, collection, &doc.id)?;
        query::delete(&tx, collection, &doc.id)?;
    }

    tx.commit().context("Commit empty trash")?;

    cli::success(&format!(
        "Permanently deleted {} document(s) from '{}'.",
        count, collection
    ));

    Ok(())
}

/// Handle the `trash` subcommand.
#[cfg(not(tarpaulin_include))]
pub fn run(action: TrashAction, config_dir: &Path) -> Result<()> {
    let config_dir = config_dir
        .canonicalize()
        .unwrap_or_else(|_| config_dir.to_path_buf());
    let (cfg, registry, pool) = init_stack(&config_dir)?;

    match action {
        TrashAction::List { collection } => run_list(&registry, &pool, &cfg, collection.as_deref()),

        TrashAction::Purge {
            collection,
            older_than,
            dry_run,
        } => run_purge(
            &registry,
            &pool,
            &config_dir,
            collection.as_deref(),
            &older_than,
            dry_run,
        ),

        TrashAction::Restore { collection, id } => run_restore(&registry, &pool, &collection, &id),

        TrashAction::Empty {
            collection,
            confirm,
        } => run_empty(&registry, &pool, &config_dir, &collection, confirm),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_older_than ──────────────────────────────────────────────────

    #[test]
    fn parse_older_than_all_returns_none() {
        assert_eq!(parse_older_than("all"), None);
    }

    #[test]
    fn parse_older_than_days() {
        assert_eq!(parse_older_than("30d"), Some(30 * 86400));
        assert_eq!(parse_older_than("7d"), Some(7 * 86400));
        assert_eq!(parse_older_than("1d"), Some(86400));
    }

    #[test]
    fn parse_older_than_hours() {
        assert_eq!(parse_older_than("24h"), Some(24 * 3600));
        assert_eq!(parse_older_than("1h"), Some(3600));
    }

    #[test]
    fn parse_older_than_minutes() {
        assert_eq!(parse_older_than("30m"), Some(30 * 60));
        assert_eq!(parse_older_than("5m"), Some(300));
    }

    #[test]
    fn parse_older_than_raw_seconds() {
        assert_eq!(parse_older_than("3600"), Some(3600));
        assert_eq!(parse_older_than("86400"), Some(86400));
    }

    #[test]
    fn parse_older_than_invalid() {
        assert_eq!(parse_older_than("abc"), None);
        assert_eq!(parse_older_than(""), None);
        assert_eq!(parse_older_than("d"), None);
    }

    #[test]
    fn parse_older_than_whitespace_trimmed() {
        assert_eq!(parse_older_than(" 30d "), Some(30 * 86400));
        assert_eq!(parse_older_than(" all "), None);
    }
}
