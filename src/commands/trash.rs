//! `trash` command — manage soft-deleted documents.

use std::{path::Path, sync::Arc};

use anyhow::{Context as _, Result, anyhow, bail};

use super::TrashAction;
use crate::{
    cli::{self, Table},
    commands::{Project, cli_find, open_project},
    config::{CrapConfig, LocaleConfig, UploadStorage},
    core::{
        CollectionDefinition, Document, Registry, upload,
        upload::{StorageBackend, create_storage_with_lease},
    },
    db::{BoxedConnection, DbConnection, DbPool, DbValue, query},
    hooks::HookRunner,
    service::{owned_file_keys, purge_document},
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
/// no auth/hook context for a CLI invocation, so we go direct to the query layer
/// through `cli_find`.
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
    let fq = deleted_filter();

    let mut table = Table::new(vec!["ID", "Title", "Collection", "Deleted At"]);
    let mut total = 0usize;

    for slug in &slugs {
        let Some(def) = registry.collections.get(slug.as_str()) else {
            continue;
        };

        let docs = cli_find(&conn, def, &fq, &cfg.locale)?;
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
    confirm: bool,
}

impl PurgeParams<'_> {
    /// Whether the purge only lists its candidates: a dry run, or a purge
    /// that was not confirmed. Nothing is deleted either way.
    fn previews_only(&self) -> bool {
        self.dry_run || !self.confirm
    }
}

/// Purge (permanently delete) trashed documents, optionally filtered by age.
/// Without `--confirm` the purge lists what it would delete and asks for the
/// flag, the same way `trash empty` does.
fn run_purge(p: &PurgeParams<'_>) -> Result<()> {
    let slugs = resolve_collections(p.registry, p.collection)?;

    if slugs.is_empty() {
        cli::info("No collections with soft_delete enabled.");
        return Ok(());
    }

    let threshold_secs = parse_threshold(p.older_than)?;

    let mut conn = p.pool.write().context("Failed to get DB connection")?;
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

        if p.previews_only() {
            for id in &ids {
                cli::info(&format!("Would purge: {slug} / {id}"));
            }

            total += ids.len() as u64;
            continue;
        }

        let skipped = purge_collection(p, &mut conn, (slug, def), &ids)?;
        total_skipped += skipped;
        total += ids.len() as u64 - skipped;
    }

    report_purge(p, total, total_skipped);

    Ok(())
}

/// Print the purge's outcome: the dry-run tally, the confirmation request,
/// or what was deleted.
fn report_purge(p: &PurgeParams<'_>, total: u64, skipped: u64) {
    if p.dry_run {
        cli::info(&format!("{total} document(s) would be purged."));
        return;
    }

    if !p.confirm {
        cli::warning(&format!(
            "This will permanently delete {total} trashed document(s)."
        ));
        cli::hint("Pass -y/--confirm to proceed.");
        return;
    }

    cli::success(&format!("Purged {total} trashed document(s)."));

    if skipped > 0 {
        cli::info(&format!(
            "{skipped} document(s) skipped — still referenced."
        ));
    }
}

/// Purge one collection's candidates in a transaction of their own, deleting
/// the upload files only once it commits: a purge that fails keeps both the
/// rows and their files. Runs on the purge's one connection. Returns the
/// number of skipped documents.
fn purge_collection(
    p: &PurgeParams<'_>,
    conn: &mut BoxedConnection,
    (slug, def): (&str, &CollectionDefinition),
    ids: &[String],
) -> Result<u64> {
    // `transaction_immediate()` — `purge_documents` issues reads (upload
    // lookups) and writes (DELETEs + FTS sync) on the same tx. DEFERRED would
    // risk `SQLITE_BUSY_SNAPSHOT` against concurrent writers.
    let tx = conn.transaction_immediate().context("Start transaction")?;
    let purged = purge_documents(&tx, (slug, def), ids, p.locale)?;
    tx.commit().context("Commit purge")?;

    delete_purged_files(p.storage, &purged);

    Ok(purged.skipped)
}

/// Delete the files of the uploads a committed purge removed.
fn delete_purged_files(storage: &dyn StorageBackend, purged: &Purged) {
    upload::delete_storage_keys(storage, &purged.upload_keys);
}

/// What purging one collection's documents did.
struct Purged {
    /// Documents skipped because others still reference them.
    skipped: u64,
    /// Every storage key the purged uploads owned — each document's row AND
    /// its version snapshots — whose files go once the purge commits.
    upload_keys: Vec<String>,
}

impl Purged {
    fn new() -> Self {
        Self {
            skipped: 0,
            upload_keys: Vec::new(),
        }
    }
}

/// Permanently delete a list of documents, cleaning up FTS and reference
/// counts, and collect the storage keys the purged uploads owned — their rows'
/// and their version snapshots' — for file cleanup. Documents that are still
/// referenced by others (`_ref_count > 0`) are skipped — the same delete
/// protection the server surfaces enforce.
fn purge_documents(
    tx: &dyn DbConnection,
    (slug, def): (&str, &CollectionDefinition),
    ids: &[String],
    locale: &LocaleConfig,
) -> Result<Purged> {
    let mut purged = Purged::new();
    // The row lookup needs the locale context: a collection with localized
    // fields has no bare columns to select.
    let locale_ctx = query::LocaleContext::default_for(locale);

    for id in ids {
        if query::ref_count::get_ref_count(tx, slug, id)?.unwrap_or(0) > 0 {
            cli::warning(&format!(
                "Skipping {slug} / {id} — still referenced by other documents"
            ));
            purged.skipped += 1;
            continue;
        }

        // Every file the document owns, its version snapshots' included —
        // collected before the purge removes the rows that name them.
        purged
            .upload_keys
            .extend(owned_file_keys(tx, def, id, locale_ctx.as_ref())?);

        purge_document(tx, def, id, locale)?;
    }

    Ok(purged)
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

    let mut conn = pool.write().context("Failed to get DB connection")?;
    // `transaction_immediate()` — avoid `SQLITE_BUSY_SNAPSHOT` against
    // concurrent writers.
    let tx = conn.transaction_immediate().context("Start transaction")?;

    let restored = query::restore(&tx, collection, id)?;

    if !restored {
        bail!("Document '{id}' not found or not in trash");
    }

    // The FTS row survives a soft delete (the trash view is searchable), so
    // nothing to re-index here.
    tx.commit().context("Commit restore")?;

    cli::success(&format!("Restored document '{id}' in '{collection}'."));

    Ok(())
}

/// Args for [`run_empty`].
struct EmptyParams<'a> {
    registry: &'a Registry,
    pool: &'a DbPool,
    storage: &'a dyn StorageBackend,
    locale: &'a LocaleConfig,
    collection: &'a str,
    confirm: bool,
}

/// Permanently delete all trashed documents in a collection.
fn run_empty(p: &EmptyParams<'_>) -> Result<()> {
    let EmptyParams {
        registry,
        pool,
        storage,
        locale,
        collection,
        confirm,
    } = *p;

    validate_soft_delete(registry, collection)?;

    let def = registry
        .collections
        .get(collection)
        .with_context(|| format!("Collection '{collection}' not found"))?
        .clone();

    let mut conn = pool.write().context("Failed to get DB connection")?;
    let docs = cli_find(&conn, &def, &deleted_filter(), locale)?;

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

    let purged = purge_documents(&tx, (collection, &def), &ids, locale)?;

    tx.commit().context("Commit empty trash")?;
    delete_purged_files(storage, &purged);

    let skipped = purged.skipped;
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
    let Project {
        lock: _instance_lock,
        config: cfg,
        registry,
        pool,
    } = open_project(&config_dir)?;

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
            confirm,
        } => run_purge(&PurgeParams {
            registry: &registry,
            pool: &pool,
            storage: &*storage,
            locale: &cfg.locale,
            collection: collection.as_deref(),
            older_than: &older_than,
            dry_run,
            confirm,
        }),

        TrashAction::Restore { collection, id } => run_restore(&registry, &pool, &collection, &id),

        TrashAction::Empty {
            collection,
            confirm,
        } => run_empty(&EmptyParams {
            registry: &registry,
            pool: &pool,
            storage: &*storage,
            locale: &cfg.locale,
            collection: &collection,
            confirm,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::DatabaseConfig,
        core::{
            JobStatus,
            field::{FieldDefinition, FieldType, RelationshipConfig},
            upload::CollectionUpload,
        },
        db::{DbValue, migrate, pool},
    };

    /// Regression: the CLI purge deleted a document without cancelling its
    /// queued image conversions, which then ran against a missing row.
    #[test]
    fn purge_cancels_queued_image_conversions() {
        let mut media = CollectionDefinition::new("media");
        media.soft_delete = true;
        media.upload = Some(CollectionUpload {
            enabled: true,
            ..Default::default()
        });
        let (_tmp, pool, _registry) = setup_db(&[media.clone()]);
        let conn = pool.get().unwrap();
        conn.execute("INSERT INTO media (id) VALUES ('m1')", &[])
            .unwrap();
        upload::queue_image_conversion(
            &conn,
            &upload::ImageConvertJobData {
                collection: "media".to_string(),
                document_id: "m1".to_string(),
                source_path: "a.png".to_string(),
                target_path: "a.webp".to_string(),
                format: "webp".to_string(),
                quality: 80,
                url_column: "thumbnail_webp_url".to_string(),
                url_value: "/uploads/a.webp".to_string(),
            },
            1,
        )
        .unwrap();

        purge_documents(
            &conn,
            ("media", &media),
            &["m1".to_string()],
            &LocaleConfig::default(),
        )
        .unwrap();

        let pending = query::jobs::count_job_runs(
            &conn,
            Some(upload::SYSTEM_IMAGE_CONVERT_JOB),
            Some(JobStatus::Pending),
        )
        .unwrap();
        assert_eq!(pending, 0);
    }

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
        let (_tmp, db_pool, _) = setup_db(&[media, posts]);

        let mut conn = db_pool.get().unwrap();
        insert_referencing_post(&conn);
        assert_eq!(ref_count(&conn, "media", "m1"), Some(1));

        let tx = conn.transaction_immediate().unwrap();
        let purged = purge_documents(
            &tx,
            ("posts", &posts_def),
            &["p1".to_string()],
            &LocaleConfig::default(),
        )
        .unwrap();
        tx.commit().unwrap();

        assert_eq!(purged.skipped, 0);
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
        let (_tmp, db_pool, _) = setup_db(&[media, posts]);

        let mut conn = db_pool.get().unwrap();
        insert_referencing_post(&conn);

        let tx = conn.transaction_immediate().unwrap();
        let purged = purge_documents(
            &tx,
            ("media", &media_def),
            &["m1".to_string()],
            &LocaleConfig::default(),
        )
        .unwrap();
        tx.commit().unwrap();

        assert_eq!(purged.skipped, 1);
        let conn = db_pool.get().unwrap();
        assert_eq!(
            ref_count(&conn, "media", "m1"),
            Some(1),
            "still-referenced m1 must survive the purge"
        );
    }

    /// Regression: purge deleted a trashed upload's files inside its
    /// transaction, so a purge that failed on a later document rolled the rows
    /// back while their files were already gone. The rows' files are now only
    /// collected in the transaction and deleted once it commits.
    #[test]
    fn purge_keeps_upload_files_until_the_purge_commits() {
        let mut media = CollectionDefinition::new("media");
        media.soft_delete = true;
        media.upload = Some(CollectionUpload::new());
        media.fields = vec![
            FieldDefinition::builder("filename", FieldType::Text).build(),
            FieldDefinition::builder("url", FieldType::Text).build(),
        ];
        let media_def = media.clone();
        let (tmp, db_pool, _) = setup_db(&[media]);

        let file = tmp.path().join("uploads").join("media").join("a.png");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, b"x").unwrap();

        let mut conn = db_pool.get().unwrap();
        conn.execute(
            "INSERT INTO media (id, filename, url, _deleted_at) VALUES \
             ('m1', 'a.png', '/uploads/media/a.png', '2026-01-01T00:00:00.000Z')",
            &[],
        )
        .unwrap();

        let tx = conn.transaction_immediate().unwrap();
        let purged = purge_documents(
            &tx,
            ("media", &media_def),
            &["m1".to_string()],
            &LocaleConfig::default(),
        )
        .unwrap();
        drop(tx);

        assert!(file.exists(), "a purge that doesn't commit keeps the file");
        assert_eq!(purged.upload_keys, vec!["media/a.png".to_string()]);

        let storage = upload::create_storage(tmp.path(), &CrapConfig::default().upload).unwrap();
        delete_purged_files(&*storage, &purged);
        assert!(!file.exists(), "the collected keys name the file to delete");
    }

    /// Regression: the purge held a pooled connection for the candidate lookup
    /// and took a second one per collection, so a pool of one connection could
    /// never purge.
    #[test]
    fn purge_runs_on_a_single_connection() {
        let mut posts = CollectionDefinition::new("posts");
        posts.soft_delete = true;

        let tmp = tempfile::tempdir().expect("tempdir");
        let config = CrapConfig {
            database: DatabaseConfig {
                path: "test.db".to_string(),
                pool_max_size: 1,
                write_pool_max_size: 1,
                connection_timeout: 1,
                ..Default::default()
            },
            ..Default::default()
        };
        let db_pool = pool::create_pool(tmp.path(), &config).expect("pool");

        let registry_shared = Registry::shared();
        registry_shared.write().unwrap().register_collection(posts);
        let registry = (*Registry::snapshot(&registry_shared)).clone();
        migrate::sync_all(&db_pool, &registry, &LocaleConfig::default()).expect("sync");

        db_pool
            .get()
            .unwrap()
            .execute(
                "INSERT INTO posts (id, _deleted_at) VALUES ('p1', '2026-01-01T00:00:00.000Z')",
                &[],
            )
            .unwrap();

        let storage = upload::create_storage(tmp.path(), &config.upload).unwrap();
        run_purge(&PurgeParams {
            registry: &registry,
            pool: &db_pool,
            storage: &*storage,
            locale: &LocaleConfig::default(),
            collection: Some("posts"),
            older_than: "all",
            dry_run: false,
            confirm: true,
        })
        .unwrap();

        let conn = db_pool.get().unwrap();
        assert!(
            conn.query_one("SELECT id FROM posts WHERE id = 'p1'", &[])
                .unwrap()
                .is_none(),
            "p1 must be purged"
        );
    }

    /// Regression: `trash purge` defaulted to every trashed document and had
    /// no confirmation flag, so a bare `crap-cms trash purge` deleted all of
    /// it. Without `--confirm` (and without `--dry-run`) it must delete
    /// nothing and ask for the flag, as `trash empty` does.
    #[test]
    fn purge_without_confirm_deletes_nothing() {
        let mut posts = CollectionDefinition::new("posts");
        posts.soft_delete = true;
        let (tmp, db_pool, registry) = setup_db(&[posts]);

        db_pool
            .get()
            .unwrap()
            .execute(
                "INSERT INTO posts (id, _deleted_at) VALUES ('p1', '2026-01-01T00:00:00.000Z')",
                &[],
            )
            .unwrap();

        let storage = upload::create_storage(tmp.path(), &CrapConfig::default().upload).unwrap();
        run_purge(&PurgeParams {
            registry: &registry,
            pool: &db_pool,
            storage: &*storage,
            locale: &LocaleConfig::default(),
            collection: Some("posts"),
            older_than: "all",
            dry_run: false,
            confirm: false,
        })
        .unwrap();

        let conn = db_pool.get().unwrap();
        assert!(
            conn.query_one("SELECT id FROM posts WHERE id = 'p1'", &[])
                .unwrap()
                .is_some(),
            "an unconfirmed purge must keep every trashed document"
        );
    }

    /// Regression: `trash empty` read the trashed documents without a locale
    /// context, so on a collection with a localized field the SELECT named a
    /// bare column that doesn't exist and the command failed.
    #[test]
    fn empty_trash_works_on_a_localized_collection() {
        let mut posts = CollectionDefinition::new("posts");
        posts.soft_delete = true;
        posts.fields = vec![
            FieldDefinition::builder("title", FieldType::Text)
                .localized(true)
                .build(),
        ];
        let locale = LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: true,
        };

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
        registry_shared.write().unwrap().register_collection(posts);
        let registry = (*Registry::snapshot(&registry_shared)).clone();
        migrate::sync_all(&db_pool, &registry, &locale).expect("sync");

        db_pool
            .get()
            .unwrap()
            .execute(
                "INSERT INTO posts (id, title__en, _deleted_at) \
                 VALUES ('p1', 'Hi', '2026-01-01T00:00:00.000Z')",
                &[],
            )
            .unwrap();

        let storage = upload::create_storage(tmp.path(), &config.upload).unwrap();
        run_empty(&EmptyParams {
            registry: &registry,
            pool: &db_pool,
            storage: &*storage,
            locale: &locale,
            collection: "posts",
            confirm: true,
        })
        .unwrap();

        let conn = db_pool.get().unwrap();
        assert!(
            conn.query_one("SELECT id FROM posts WHERE id = 'p1'", &[])
                .unwrap()
                .is_none()
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
