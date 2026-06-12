//! `trash` command — manage soft-deleted documents.

use std::{path::Path, sync::Arc};

use anyhow::{Context as _, Result, anyhow, bail};

use super::TrashAction;
use crate::{
    cli::{self, Table},
    commands::helpers::init_stack,
    config::{CrapConfig, LocaleConfig, UploadStorage},
    core::{
        CollectionDefinition, Document, Registry, upload,
        upload::{StorageBackend, create_storage_with_lease},
    },
    db::{DbConnection, DbPool, DbValue, query},
    hooks::HookRunner,
};

/// Validate that a collection exists and has `soft_delete` enabled.
fn validate_soft_delete(registry: &Registry, slug: &str) -> Result<()> {
    let def = registry
        .collections
        .get(slug)
        .ok_or_else(|| anyhow!("Collection '{slug}' not found"))?;

    if !def.soft_delete {
        bail!("Collection '{slug}' does not have soft_delete enabled");
    }

    Ok(())
}

/// Collect slugs of collections that have `soft_delete = true`.
/// If `filter` is provided, only return that collection (validating it exists and supports soft delete).
fn resolve_collections(registry: &Registry, filter: Option<&str>) -> Result<Vec<String>> {
    if let Some(slug) = filter {
        validate_soft_delete(registry, slug)?;
        return Ok(vec![slug.to_string()]);
    }

    let mut slugs: Vec<String> = registry
        .collections
        .iter()
        .filter(|(_, def)| def.soft_delete)
        .map(|(slug, _)| slug.to_string())
        .collect();

    slugs.sort();

    Ok(slugs)
}

/// Build a `FindQuery` that returns only soft-deleted documents.
///
/// CLI bypasses the service layer (`find_documents`) intentionally — there is
/// no auth/hook context for a CLI invocation, so we go direct to `query::find`.
/// The trade-off: this `_deleted_at EXISTS` filter is an internal injection,
/// not a user filter, so it sidesteps the service-layer validator. Keep this
/// helper private to the CLI so the bypass stays scoped.
fn deleted_filter() -> query::FindQuery {
    query::FindQuery::builder()
        .include_deleted(true)
        .filters(vec![query::FilterClause::Single(query::Filter {
            field: "_deleted_at".to_string(),
            op: query::FilterOp::Exists,
        })])
        .build()
}

/// List trashed (soft-deleted) documents across collections.
fn run_list(
    registry: &Registry,
    pool: &DbPool,
    cfg: &CrapConfig,
    collection: Option<&str>,
) -> Result<()> {
    let slugs = resolve_collections(registry, collection)?;

    if slugs.is_empty() {
        cli::info("No collections with soft_delete enabled.");
        return Ok(());
    }

    let conn = pool.get().context("Failed to get DB connection")?;
    let locale_ctx = query::LocaleContext::from_locale_string(None, &cfg.locale)?;
    let fq = deleted_filter();

    let mut table = Table::new(vec!["ID", "Title", "Collection", "Deleted At"]);
    let mut total = 0usize;

    for slug in &slugs {
        let Some(def) = registry.collections.get(slug.as_str()) else {
            continue;
        };

        let docs = query::find(&conn, slug, def, &fq, locale_ctx.as_ref())?;
        total += collect_trash_rows(&mut table, &docs, slug, def.title_field().unwrap_or("id"));
    }

    if total == 0 {
        cli::info("No trashed documents found.");
    } else {
        table.print();
        table.footer(&format!("{total} trashed document(s)"));
    }

    Ok(())
}

/// Append trashed document rows to the table, returns the count added.
fn collect_trash_rows(
    table: &mut Table,
    docs: &[Document],
    slug: &str,
    title_field: &str,
) -> usize {
    for doc in docs {
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
    }

    docs.len()
}

/// Parse a duration string like "30d", "7d", "24h" into seconds.
///
/// Returns `None` for "all", invalid input, or a value that overflows `i64`
/// when multiplied by its unit factor (e.g. `i64::MAX d`).
fn parse_older_than(s: &str) -> Option<i64> {
    let s = s.trim();

    if s == "all" {
        return None;
    }

    if let Some(days) = s.strip_suffix('d') {
        days.parse::<i64>().ok().and_then(|d| d.checked_mul(86400))
    } else if let Some(hours) = s.strip_suffix('h') {
        hours.parse::<i64>().ok().and_then(|h| h.checked_mul(3600))
    } else if let Some(mins) = s.strip_suffix('m') {
        mins.parse::<i64>().ok().and_then(|m| m.checked_mul(60))
    } else {
        s.parse::<i64>().ok()
    }
}

/// Parse the `older_than` arg into an optional threshold in seconds.
fn parse_threshold(older_than: &str) -> Result<Option<i64>> {
    if older_than == "all" {
        return Ok(None);
    }

    let secs = parse_older_than(older_than).ok_or_else(|| {
        anyhow!(
            "Invalid duration '{older_than}'. Use format like '30d' (days), '24h' (hours), '30m' (minutes), '60s' (seconds), or 'all'"
        )
    })?;

    Ok(Some(secs))
}

/// Args for [`run_purge`]. Bundles the runtime handles and the
/// `TrashAction::Purge` variant fields so the call site reads
/// declaratively rather than positionally.
struct PurgeParams<'a> {
    registry: &'a Registry,
    pool: &'a DbPool,
    storage: &'a dyn StorageBackend,
    locale: &'a LocaleConfig,
    collection: Option<&'a str>,
    older_than: &'a str,
    dry_run: bool,
}

/// Purge (permanently delete) trashed documents, optionally filtered by age.
fn run_purge(p: &PurgeParams<'_>) -> Result<()> {
    let slugs = resolve_collections(p.registry, p.collection)?;

    if slugs.is_empty() {
        cli::info("No collections with soft_delete enabled.");
        return Ok(());
    }

    let threshold_secs = parse_threshold(p.older_than)?;

    let mut conn = p.pool.get().context("Failed to get DB connection")?;
    let mut total = 0u64;
    let mut total_skipped = 0u64;

    for slug in &slugs {
        let Some(def) = p.registry.collections.get(slug.as_str()) else {
            continue;
        };

        let ids = find_purge_candidates(&conn as &dyn DbConnection, slug, threshold_secs)?;

        if ids.is_empty() {
            continue;
        }

        if p.dry_run {
            for id in &ids {
                cli::info(&format!("Would purge: {slug} / {id}"));
            }
        } else {
            // `transaction_immediate()` — `purge_documents` issues
            // reads (find_by_id_unfiltered to look up upload paths) and
            // writes (DELETEs + FTS sync) on the same tx. DEFERRED would
            // risk `SQLITE_BUSY_SNAPSHOT` against concurrent writers.
            let tx = conn.transaction_immediate().context("Start transaction")?;
            let skipped = purge_documents(&tx, slug, def, &ids, p.storage, p.locale)?;
            tx.commit().context("Commit purge")?;

            // Re-acquire connection after commit (tx consumed it)
            conn = p.pool.get().context("Failed to get DB connection")?;

            total_skipped += skipped;
            total += ids.len() as u64 - skipped;
            continue;
        }

        total += ids.len() as u64;
    }

    if p.dry_run {
        cli::info(&format!("{total} document(s) would be purged."));
    } else {
        cli::success(&format!("Purged {total} trashed document(s)."));
        if total_skipped > 0 {
            cli::info(&format!(
                "{total_skipped} document(s) skipped — still referenced."
            ));
        }
    }

    Ok(())
}

/// Permanently delete a list of documents, cleaning up uploads, FTS, and
/// reference counts. Documents that are still referenced by others
/// (`_ref_count > 0`) are skipped — the same delete protection the server
/// surfaces enforce. Returns the number of skipped documents.
fn purge_documents(
    tx: &dyn DbConnection,
    slug: &str,
    def: &CollectionDefinition,
    ids: &[String],
    storage: &dyn StorageBackend,
    locale: &LocaleConfig,
) -> Result<u64> {
    let mut skipped = 0u64;

    for id in ids {
        if query::ref_count::get_ref_count(tx, slug, id)?.unwrap_or(0) > 0 {
            cli::warning(&format!(
                "Skipping {slug} / {id} — still referenced by other documents"
            ));
            skipped += 1;
            continue;
        }

        if def.is_upload_collection()
            && let Ok(Some(doc)) = query::find_by_id_unfiltered(tx, slug, def, id, None)
        {
            upload::delete_upload_files(storage, &doc.fields);
        }

        query::ref_count::before_hard_delete(tx, slug, id, &def.fields, locale)?;
        query::fts::fts_delete(tx, slug, id)?;
        query::delete(tx, slug, id)?;
    }

    Ok(skipped)
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
                    "SELECT id FROM \"{slug}\" WHERE _deleted_at IS NOT NULL \
                     AND _deleted_at < {offset_sql}"
                ),
                vec![offset_param],
            )
        }
        None => (
            format!("SELECT id FROM \"{slug}\" WHERE _deleted_at IS NOT NULL"),
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
fn run_restore(registry: &Registry, pool: &DbPool, collection: &str, id: &str) -> Result<()> {
    validate_soft_delete(registry, collection)?;

    let def = registry
        .collections
        .get(collection)
        .with_context(|| format!("Collection '{collection}' not found"))?;

    let mut conn = pool.get().context("Failed to get DB connection")?;
    // `transaction_immediate()` — restore reads (find_by_id_unfiltered
    // for the FTS re-sync) and writes (UPDATE deleted_at, FTS upsert)
    // on the same tx. Avoid `SQLITE_BUSY_SNAPSHOT` against concurrent
    // writers.
    let tx = conn.transaction_immediate().context("Start transaction")?;

    let restored = query::restore(&tx, collection, id)?;

    if !restored {
        bail!("Document '{id}' not found or not in trash");
    }

    // Re-sync FTS index (FTS row was deleted on soft-delete)
    if tx.supports_fts()
        && let Ok(Some(doc)) = query::find_by_id_unfiltered(&tx, collection, def, id, None)
    {
        query::fts::fts_upsert(&tx, collection, &doc, Some(def))?;
    }

    tx.commit().context("Commit restore")?;

    cli::success(&format!("Restored document '{id}' in '{collection}'."));

    Ok(())
}

/// Permanently delete all trashed documents in a collection.
fn run_empty(
    registry: &Registry,
    pool: &DbPool,
    storage: &dyn StorageBackend,
    locale: &LocaleConfig,
    collection: &str,
    confirm: bool,
) -> Result<()> {
    validate_soft_delete(registry, collection)?;

    let def = registry
        .collections
        .get(collection)
        .with_context(|| format!("Collection '{collection}' not found"))?
        .clone();

    let mut conn = pool.get().context("Failed to get DB connection")?;
    let fq = deleted_filter();
    let docs = query::find(&conn, collection, &def, &fq, None)?;

    if docs.is_empty() {
        cli::info(&format!("No trashed documents in '{collection}'."));
        return Ok(());
    }

    if !confirm {
        cli::warning(&format!(
            "This will permanently delete {} document(s) from '{}'.",
            docs.len(),
            collection
        ));
        cli::hint("Pass -y/--confirm to proceed.");
        return Ok(());
    }

    let ids: Vec<String> = docs.iter().map(|d| d.id.to_string()).collect();
    // `transaction_immediate()` — `purge_documents` interleaves reads
    // (upload path lookups) and writes (DELETEs + FTS sync) on the
    // same tx. See the matching note in `run_purge`.
    let tx = conn.transaction_immediate().context("Start transaction")?;

    let skipped = purge_documents(&tx, collection, &def, &ids, storage, locale)?;

    tx.commit().context("Commit empty trash")?;

    cli::success(&format!(
        "Permanently deleted {} document(s) from '{}'.",
        ids.len() as u64 - skipped,
        collection
    ));
    if skipped > 0 {
        cli::info(&format!(
            "{skipped} document(s) skipped — still referenced."
        ));
    }

    Ok(())
}

/// Handle the `trash` subcommand.
///
/// # Errors
///
/// Returns an error if config loading, pool creation, storage init, or the
/// dispatched action fails.
#[cfg(not(tarpaulin_include))]
pub fn run(action: TrashAction, config_dir: &Path) -> Result<()> {
    let config_dir = config_dir
        .canonicalize()
        .unwrap_or_else(|_| config_dir.to_path_buf());
    let (cfg, registry, pool) = init_stack(&config_dir)?;

    // A custom storage backend delegates to Lua, so it needs a VM pool.
    // Build a hook runner only in that case; the lease keeps the pool
    // alive after the runner is dropped (it holds an Arc to the pool).
    let storage = if matches!(cfg.upload.storage, UploadStorage::Custom) {
        let hook_runner = HookRunner::builder()
            .config_dir(&config_dir)
            .registry(Arc::clone(&registry))
            .config(&cfg)
            .build()?;
        create_storage_with_lease(&config_dir, &cfg.upload, hook_runner.lua_lease())?
    } else {
        upload::create_storage(&config_dir, &cfg.upload)?
    };

    match action {
        TrashAction::List { collection } => run_list(&registry, &pool, &cfg, collection.as_deref()),

        TrashAction::Purge {
            collection,
            older_than,
            dry_run,
        } => run_purge(&PurgeParams {
            registry: &registry,
            pool: &pool,
            storage: &*storage,
            locale: &cfg.locale,
            collection: collection.as_deref(),
            older_than: &older_than,
            dry_run,
        }),

        TrashAction::Restore { collection, id } => run_restore(&registry, &pool, &collection, &id),

        TrashAction::Empty {
            collection,
            confirm,
        } => run_empty(
            &registry,
            &pool,
            &*storage,
            &cfg.locale,
            &collection,
            confirm,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::DatabaseConfig,
        core::field::{FieldDefinition, FieldType, RelationshipConfig},
        db::{DbValue, migrate, pool},
    };

    // ── purge_documents ref-count semantics ──────────────────────────────

    fn setup_db(collections: &[CollectionDefinition]) -> (tempfile::TempDir, DbPool, Registry) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config = CrapConfig {
            database: DatabaseConfig {
                path: "test.db".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };
        let db_pool = pool::create_pool(tmp.path(), &config).expect("pool");

        let registry_shared = Registry::shared();
        {
            let mut reg = registry_shared.write().unwrap();
            for c in collections {
                reg.register_collection(c.clone());
            }
        }
        let registry = (*Registry::snapshot(&registry_shared)).clone();
        migrate::sync_all(&db_pool, &registry, &LocaleConfig::default()).expect("sync");

        (tmp, db_pool, registry)
    }

    fn defs_with_relationship() -> (CollectionDefinition, CollectionDefinition) {
        let media = CollectionDefinition::new("media");
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = vec![
            FieldDefinition::builder("image", FieldType::Relationship)
                .relationship(RelationshipConfig::new("media", false))
                .build(),
        ];
        (media, posts)
    }

    fn insert_referencing_post(conn: &dyn DbConnection) {
        conn.execute(
            "INSERT INTO media (id) VALUES (?1)",
            &[DbValue::Text("m1".into())],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO posts (id, image) VALUES (?1, ?2)",
            &[DbValue::Text("p1".into()), DbValue::Text("m1".into())],
        )
        .unwrap();
        query::ref_count::after_create(
            conn,
            "posts",
            "p1",
            &[FieldDefinition::builder("image", FieldType::Relationship)
                .relationship(RelationshipConfig::new("media", false))
                .build()],
            &LocaleConfig::default(),
        )
        .unwrap();
    }

    fn ref_count(conn: &dyn DbConnection, table: &str, id: &str) -> Option<i64> {
        query::ref_count::get_ref_count(conn, table, id).unwrap()
    }

    /// Regression: purging a trashed document must decrement the ref counts
    /// of the documents it references — the raw-delete path used to skip
    /// `before_hard_delete`, leaving targets with inflated `_ref_count`.
    #[test]
    fn purge_decrements_referenced_targets() {
        let (media, posts) = defs_with_relationship();
        let posts_def = posts.clone();
        let (tmp, db_pool, _) = setup_db(&[media, posts]);
        let storage = upload::create_storage(tmp.path(), &CrapConfig::default().upload).unwrap();

        let mut conn = db_pool.get().unwrap();
        insert_referencing_post(&conn);
        assert_eq!(ref_count(&conn, "media", "m1"), Some(1));

        let tx = conn.transaction_immediate().unwrap();
        let skipped = purge_documents(
            &tx,
            "posts",
            &posts_def,
            &["p1".to_string()],
            &*storage,
            &LocaleConfig::default(),
        )
        .unwrap();
        tx.commit().unwrap();

        assert_eq!(skipped, 0);
        let conn = db_pool.get().unwrap();
        assert_eq!(ref_count(&conn, "media", "m1"), Some(0));
        assert_eq!(ref_count(&conn, "posts", "p1"), None, "p1 must be gone");
    }

    /// Regression: purging must skip documents that are still referenced by
    /// others — the raw-delete path used to bypass delete protection.
    #[test]
    fn purge_skips_still_referenced_documents() {
        let (media, posts) = defs_with_relationship();
        let media_def = media.clone();
        let (tmp, db_pool, _) = setup_db(&[media, posts]);
        let storage = upload::create_storage(tmp.path(), &CrapConfig::default().upload).unwrap();

        let mut conn = db_pool.get().unwrap();
        insert_referencing_post(&conn);

        let tx = conn.transaction_immediate().unwrap();
        let skipped = purge_documents(
            &tx,
            "media",
            &media_def,
            &["m1".to_string()],
            &*storage,
            &LocaleConfig::default(),
        )
        .unwrap();
        tx.commit().unwrap();

        assert_eq!(skipped, 1);
        let conn = db_pool.get().unwrap();
        assert_eq!(
            ref_count(&conn, "media", "m1"),
            Some(1),
            "still-referenced m1 must survive the purge"
        );
    }

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

    /// BUG-4 regression: a value that would overflow i64 when multiplied by
    /// its unit factor must return None instead of silently wrapping.
    #[test]
    fn parse_older_than_overflow_errors() {
        // i64::MAX days × 86400 overflows.
        assert_eq!(parse_older_than(&format!("{}d", i64::MAX)), None);
        assert_eq!(parse_older_than(&format!("{}h", i64::MAX)), None);
        assert_eq!(parse_older_than(&format!("{}m", i64::MAX)), None);
        // Bare seconds fit (parse returns the number as-is without multiply).
        assert_eq!(parse_older_than(&i64::MAX.to_string()), Some(i64::MAX));
    }
}
