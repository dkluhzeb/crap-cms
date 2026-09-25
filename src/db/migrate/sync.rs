//! Top-level schema sync: creates system tables and syncs all collections/globals.

use anyhow::{Context as _, Result, bail};
use tracing::error;

use crate::{
    config::LocaleConfig,
    core::{
        Registry, ScheduledBy,
        upload::{
            FALLBACK_MAX_ATTEMPTS, IMAGE_CONVERT_QUEUE, ImageConvertJobData,
            SYSTEM_IMAGE_CONVERT_JOB,
        },
    },
    db::{
        BoxedConnection, BoxedTransaction, DbConnection, DbPool,
        query::{
            fts::{FtsIndex, sync_fts_table},
            helpers::global_table,
            jobs as job_query,
        },
    },
};

use super::{
    backfill_ref_counts, canonical_text, checkbox_columns, collection, global, has_many_lists,
    helpers::{get_table_columns, table_exists},
    identifier_check, legacy_timestamps, locale_change, meta, nested_values,
    orphan_tables::warn_orphan_tables,
    reference_cardinality,
    tracking::drop_all_tables,
};

/// Sync all collection tables with their Lua definitions.
///
/// Concurrency safety: `transaction_immediate()` acquires `SQLite`'s write lock at
/// transaction start (not first write), so concurrent `sync_all` calls are serialized
/// by the database engine. Combined with `busy_timeout` (default 30s), the second caller
/// waits rather than failing.
///
/// # Errors
///
/// Returns an error if the connection, transaction, or any of the
/// per-collection/global schema-sync steps fails.
pub fn sync_all(pool: &DbPool, registry: &Registry, locale_config: &LocaleConfig) -> Result<()> {
    let mut conn = pool.write().context("Failed to get DB connection")?;

    // A constraint change (turning on `soft_delete`, relaxing an old NOT NULL)
    // rebuilds the table on SQLite, and a rebuild only keeps the children's
    // rows and their foreign keys with enforcement off (see
    // `collection::rebuild`). `PRAGMA foreign_keys` is
    // ignored inside a transaction, so the window has to be opened here, and
    // only when a rebuild is actually pending — an ordinary boot syncs with
    // enforcement on, as always.
    let rebuilt = pending_rebuilds(&conn, registry, locale_config)?;

    if !rebuilt.is_empty() {
        set_foreign_keys(&conn, false)?;
    }

    let result = run_sync(&mut conn, registry, locale_config, &rebuilt);

    if rebuilt.is_empty() {
        return result;
    }

    // Restored whether or not the sync succeeded: a failed sync rolled its
    // transaction back, and the connection goes back to the pool either way.
    // The pool applies its pragmas when a connection is created, not on every
    // checkout, so a connection left with enforcement off would serve that
    // way for its lifetime — a restore that fails has to fail the boot.
    match (result, set_foreign_keys(&conn, true)) {
        (Err(sync), Err(restore)) => {
            error!("Failed to re-enable foreign keys after the failed schema sync: {restore:#}");
            Err(sync)
        }
        (Ok(()), Err(restore)) => Err(restore.context(
            "Foreign-key enforcement could not be restored after the schema sync; \
             refusing to serve with it off",
        )),
        (result, Ok(())) => result,
    }
}

/// The collections whose table still has a constraint change pending that a
/// rebuild has to carry out — the `soft_delete` transition or the one-time
/// `NOT NULL` relax. `SQLite` only: Postgres changes the constraints in place
/// and never rebuilds, so it needs no window.
fn pending_rebuilds(
    conn: &dyn DbConnection,
    registry: &Registry,
    locale_config: &LocaleConfig,
) -> Result<Vec<String>> {
    if !conn.is_sqlite() {
        return Ok(Vec::new());
    }

    let mut rebuilt = Vec::new();

    for (slug, def) in &registry.collections {
        if !table_exists(conn, slug)? {
            continue;
        }

        let existing = get_table_columns(conn, slug)?;

        if collection::PendingConstraints::read(conn, slug, def, &existing, locale_config)?
            .needs_rebuild()
        {
            rebuilt.push(slug.to_string());
        }
    }

    Ok(rebuilt)
}

/// Turn `SQLite`'s foreign-key enforcement on or off for this connection.
fn set_foreign_keys(conn: &dyn DbConnection, on: bool) -> Result<()> {
    let state = if on { "ON" } else { "OFF" };

    conn.execute_batch(&format!("PRAGMA foreign_keys = {state}"))
        .with_context(|| format!("Failed to set foreign_keys = {state}"))
}

/// Fail the sync rather than commit a table rebuild that left a child row
/// pointing at nothing. Only run when enforcement was off for a rebuild. The
/// check scans the whole database, so only rows whose parent is a rebuilt
/// table count — an orphan that predates this boot in an unrelated table is
/// not the sync's doing and must not block it.
fn assert_no_dangling_references(conn: &dyn DbConnection, rebuilt: &[String]) -> Result<()> {
    let rows = conn
        .query_all("PRAGMA foreign_key_check", &[])
        .context("Failed to verify foreign keys after the schema sync")?;

    let mut dangling: Vec<String> = rows
        .iter()
        .filter(|row| {
            row.get_string("parent")
                .is_ok_and(|parent| rebuilt.contains(&parent))
        })
        .filter_map(|row| row.get_string("table").ok())
        .collect();
    dangling.sort();
    dangling.dedup();

    if !dangling.is_empty() {
        bail!(
            "Schema sync left rows in {} pointing at a rebuilt table; rolling back",
            dangling.join(", ")
        );
    }

    Ok(())
}

/// The schema sync proper, inside one transaction.
fn run_sync(
    conn: &mut BoxedConnection,
    registry: &Registry,
    locale_config: &LocaleConfig,
    rebuilt: &[String],
) -> Result<()> {
    let tx = open_schema_transaction(conn)?;

    sync_in_transaction(&tx, registry, locale_config, rebuilt)?;

    tx.commit()
        .context("Failed to commit migration transaction")?;

    Ok(())
}

/// Open the transaction a schema change runs in, holding the schema-sync lock.
///
/// Nodes booting together would otherwise race the same DDL on Postgres
/// (duplicate CREATE TABLE / ALTER TABLE ADD COLUMN) and crash all but one.
/// Released at commit; a no-op on `SQLite`, whose IMMEDIATE transaction
/// already serializes writers.
fn open_schema_transaction(conn: &mut BoxedConnection) -> Result<BoxedTransaction<'_>> {
    let tx = conn
        .transaction_immediate()
        .context("Failed to start migration transaction")?;

    tx.advisory_xact_lock(SCHEMA_SYNC_LOCK_KEY)
        .context("Failed to acquire the schema-sync lock")?;

    Ok(tx)
}

/// Drop every table and recreate the schema from the definitions — `migrate
/// fresh` — in one transaction under the schema-sync lock.
///
/// Both backends run DDL transactionally, so a failure anywhere leaves the
/// database exactly as it was rather than half-dropped, and a node syncing
/// its schema at the same time waits for the lock instead of interleaving
/// with the drop. Other nodes' running servers are not stopped by anything
/// here: they see the old schema until the commit and an empty one after.
///
/// # Errors
///
/// Returns an error if the connection, the drop or the schema sync fails;
/// nothing is committed then.
pub fn recreate_all(
    pool: &DbPool,
    registry: &Registry,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let mut conn = pool.write().context("Failed to get DB connection")?;
    let tx = open_schema_transaction(&mut conn)?;

    drop_all_tables(&tx)?;
    sync_in_transaction(&tx, registry, locale_config, &[])?;

    tx.commit()
        .context("Failed to commit the recreated schema")?;

    Ok(())
}

/// Every step of the schema sync, on `tx`.
fn sync_in_transaction(
    tx: &dyn DbConnection,
    registry: &Registry,
    locale_config: &LocaleConfig,
    rebuilt: &[String],
) -> Result<()> {
    create_system_tables(tx)?;
    check_all_identifiers(registry, locale_config)?;
    sync_tables(tx, registry, locale_config)?;

    delete_retired_meta_keys(tx)?;

    // Reported, never removed: a table that fell out of the registry may hold
    // the only copy of its data, so the drop is an explicit operator decision.
    warn_orphan_tables(tx, registry)?;

    run_conversions(tx, registry, locale_config)?;

    if !rebuilt.is_empty() {
        assert_no_dangling_references(tx, rebuilt)?;
    }

    Ok(())
}

/// Portability guard: reject any generated identifier that would overflow
/// Postgres's 63-byte limit (and silently truncate/collide) BEFORE creating
/// any table — on every backend, so it surfaces in `SQLite` development.
fn check_all_identifiers(registry: &Registry, locale_config: &LocaleConfig) -> Result<()> {
    for (slug, def) in &registry.collections {
        identifier_check::check_identifiers(slug, &def.fields, locale_config)?;
        identifier_check::check_index_names(slug, def, locale_config)?;
    }

    identifier_check::check_index_name_collisions(registry, locale_config)?;

    for (slug, def) in &registry.globals {
        let table = global_table(slug);
        identifier_check::check_identifiers(&table, &def.fields, locale_config)?;
    }

    Ok(())
}

/// Create or alter every collection's and global's tables.
fn sync_tables(
    tx: &dyn DbConnection,
    registry: &Registry,
    locale_config: &LocaleConfig,
) -> Result<()> {
    // Before the per-collection sync, which refuses to serve a column whose
    // type no longer matches its field: a Postgres database created by an
    // older release still has its checkbox columns as BIGINT, and this is the
    // pass that brings them to the type the definitions ask for.
    checkbox_columns::migrate_if_needed(tx, registry)?;

    for (slug, def) in &registry.collections {
        collection::sync_collection_table(tx, slug, def, locale_config)?;

        // Rebuilt with the registry, so rich text custom nodes contribute
        // their `searchable_attrs` exactly as the per-write upsert indexes them.
        if tx.supports_fts() {
            let index = FtsIndex::builder(slug, def, locale_config)
                .registry(Some(registry))
                .build();
            sync_fts_table(tx, &index)?;
        }
    }

    for (slug, def) in &registry.globals {
        global::sync_global_table(tx, slug, def, locale_config)?;
    }

    Ok(())
}

/// The one-time conversions and the stored-shape passes, after the tables
/// match the definitions.
fn run_conversions(
    tx: &dyn DbConnection,
    registry: &Registry,
    locale_config: &LocaleConfig,
) -> Result<()> {
    // Values of a reference whose `has_many` flipped move between its column
    // and its junction first, so the recount below counts them where they are
    // read from now.
    reference_cardinality::carry_if_needed(tx, registry, locale_config)?;

    // The one-time conversions run in this order on purpose: nested values
    // are typed before text is canonicalized, since both rewrite the same
    // JSON-stored rows and the canonical form applies to the typed value.
    backfill_ref_counts::backfill_if_needed(tx, registry, locale_config)?;
    legacy_timestamps::normalize_if_needed(tx, registry)?;

    // Filters expand every stored has-many list, so a value a definition change
    // left behind is stored as a list before anything reads it. It runs before
    // the nested values are typed: typing a nested value that isn't a list yet
    // would drop what doesn't fit the field's type instead of refusing it.
    has_many_lists::normalize_if_needed(tx, registry, locale_config)?;
    nested_values::convert_if_needed(tx, registry)?;
    canonical_text::canonicalize_if_needed(tx, registry, locale_config)?;

    locale_change::warn_on_default_locale_change(tx, registry, locale_config)
}

/// Advisory-lock key serializing schema sync across nodes: the ASCII bytes of
/// `"crapsync"`, distinct from the job-claim key.
const SCHEMA_SYNC_LOCK_KEY: i64 = i64::from_be_bytes(*b"crapsync");

/// The `_crap_meta` gates of migrations that no longer exist, removed so a
/// database carries no key naming a pass nothing reads any more.
///
/// `ref_count_backfilled` was the whole-database flag the per-slug
/// `ref_count_backfilled:{slug}` gates replaced — a collection added after it
/// was stamped had its reference counts skipped, which is why it went per-slug.
/// A conversion whose gate is per-slug removes its own replaced keys, which
/// need the registry to name; this is where the ones that don't go.
const RETIRED_META_KEYS: &[&str] = &["ref_count_backfilled"];

/// Remove the gates listed in [`RETIRED_META_KEYS`]. Deleting an absent key
/// changes nothing, so this is a no-op on every database that never held them.
fn delete_retired_meta_keys(conn: &dyn DbConnection) -> Result<()> {
    for key in RETIRED_META_KEYS {
        meta::delete(conn, key)
            .with_context(|| format!("Failed to remove the retired meta key {key}"))?;
    }

    Ok(())
}

/// Create all system tables (_`crap_meta`, _`crap_migrations`, _`crap_jobs`, etc.).
fn create_system_tables(conn: &dyn DbConnection) -> Result<()> {
    let td = conn.timestamp_column_default();
    let tt = conn.timestamp_column_type();

    conn.execute_batch_ddl(&format!(
        "CREATE TABLE IF NOT EXISTS _crap_meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL,
            updated_at {td}
        );"
    ))
    .context("Failed to create _crap_meta table")?;

    conn.execute_batch_ddl(&format!(
        "CREATE TABLE IF NOT EXISTS _crap_migrations (
            filename TEXT PRIMARY KEY,
            applied_at {td}
        );"
    ))
    .context("Failed to create _crap_migrations table")?;

    conn.execute_batch_ddl(&format!(
        "CREATE TABLE IF NOT EXISTS _crap_cron_fired (
            slug TEXT PRIMARY KEY,
            fired_at {td}
        );"
    ))
    .context("Failed to create _crap_cron_fired table")?;

    conn.execute_batch_ddl(
        "CREATE TABLE IF NOT EXISTS _crap_user_settings (
            user_id TEXT PRIMARY KEY,
            settings TEXT NOT NULL DEFAULT '{}'
        );",
    )
    .context("Failed to create _crap_user_settings table")?;

    create_jobs_table(conn, td, tt)?;

    // alpha.9: image conversion moved into the unified job queue.
    // Drain any in-flight rows from the legacy `_crap_image_queue` into
    // `_crap_jobs` as `_system_image_convert` system jobs, then drop
    // the legacy table. Idempotent — skips if the table is already
    // gone.
    drain_legacy_image_queue(conn)?;

    Ok(())
}

/// Create the jobs table and ensure schema migrations.
///
/// Single source of truth for the `_crap_jobs` schema — production
/// migration AND test setups (`test_helpers::setup_db`,
/// `scheduler/runner.rs` tests) call this directly so the schema
/// can't drift between paths. The `CREATE TABLE IF NOT EXISTS` makes
/// it safe to call on already-migrated databases; subsequent
/// `ALTER ADD COLUMN` blocks are also idempotent.
pub(crate) fn create_jobs_table(
    tx: &dyn DbConnection,
    ts_default: &str,
    ts_type: &str,
) -> Result<()> {
    // Step 1: Create the table and the indexes that reference columns
    // ALL alpha versions have. New columns added in later alphas
    // (`retry_after`, `priority`, `unique_key`) are listed in the
    // CREATE TABLE schema so fresh databases get them right away;
    // their indexes are deferred to step 3 so an upgrade path (where
    // CREATE TABLE IF NOT EXISTS is a no-op against a table missing
    // those columns) doesn't crash trying to index a column that
    // doesn't exist yet.
    tx.execute_batch_ddl(&format!(
        "CREATE TABLE IF NOT EXISTS _crap_jobs (
            id TEXT PRIMARY KEY,
            slug TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT 'pending',
            queue TEXT NOT NULL DEFAULT 'default',
            data TEXT DEFAULT '{{}}',
            result TEXT,
            error TEXT,
            attempt INTEGER NOT NULL DEFAULT 0,
            max_attempts INTEGER NOT NULL DEFAULT 1,
            priority INTEGER NOT NULL DEFAULT 0,
            unique_key TEXT,
            scheduled_by TEXT,
            created_at {ts_default},
            started_at {ts_type},
            completed_at {ts_type},
            heartbeat_at {ts_type},
            retry_after {ts_type}
        );
        CREATE INDEX IF NOT EXISTS idx_crap_jobs_status ON _crap_jobs(status);
        CREATE INDEX IF NOT EXISTS idx_crap_jobs_queue ON _crap_jobs(queue, status);
        CREATE INDEX IF NOT EXISTS idx_crap_jobs_slug ON _crap_jobs(slug, status);"
    ))
    .context("Failed to create _crap_jobs table")?;

    // Step 2: Ensure newer columns exist on upgrade paths. Each ALTER
    // is idempotent via the `job_cols` check.
    let job_cols = tx.get_table_columns("_crap_jobs")?;

    // Added in 0.1.0-alpha.3
    if !job_cols.contains("retry_after") {
        tx.execute_batch_ddl("ALTER TABLE _crap_jobs ADD COLUMN retry_after TEXT")
            .context("Failed to add retry_after column to _crap_jobs")?;
    }

    // Added in 0.1.0-alpha.9
    if !job_cols.contains("priority") {
        tx.execute_batch_ddl(
            "ALTER TABLE _crap_jobs ADD COLUMN priority INTEGER NOT NULL DEFAULT 0",
        )
        .context("Failed to add priority column to _crap_jobs")?;
    }

    // Added in 0.1.0-alpha.9. The partial unique index (step 3) makes
    // `(slug, unique_key)` unique among pending+running rows only —
    // completed/failed runs don't block re-enqueue of the same
    // logical task.
    if !job_cols.contains("unique_key") {
        tx.execute_batch_ddl("ALTER TABLE _crap_jobs ADD COLUMN unique_key TEXT")
            .context("Failed to add unique_key column to _crap_jobs")?;
    }

    // Step 3: Create indexes that reference columns added in later
    // alphas. Run AFTER the ALTER blocks so the columns are
    // guaranteed to exist. `IF NOT EXISTS` keeps these idempotent.
    tx.execute_batch_ddl(
        "CREATE INDEX IF NOT EXISTS idx_crap_jobs_priority \
            ON _crap_jobs(status, queue, priority DESC, created_at);
         CREATE UNIQUE INDEX IF NOT EXISTS idx_crap_jobs_unique_active \
            ON _crap_jobs(slug, unique_key) \
            WHERE unique_key IS NOT NULL AND status IN ('pending', 'running');",
    )
    .context("Failed to create alpha.9 indexes on _crap_jobs")?;

    Ok(())
}

/// One-time alpha.9 migration: move any non-completed
/// `_crap_image_queue` rows into the unified `_crap_jobs` table as
/// `_system_image_convert` system jobs, then drop the legacy table.
///
/// Idempotent: returns early if `_crap_image_queue` doesn't exist.
fn drain_legacy_image_queue(conn: &dyn DbConnection) -> Result<()> {
    // SQLite-only check; on Postgres this code path never fires
    // because alpha.9 schemas there were never deployed.
    if !conn.is_sqlite() {
        return Ok(());
    }

    let Ok(rows) = conn.query_all(
        "SELECT name FROM sqlite_master WHERE type='table' AND name='_crap_image_queue'",
        &[],
    ) else {
        return Ok(());
    };
    if rows.is_empty() {
        return Ok(());
    }

    // Move every non-completed row into `_crap_jobs`. We deliberately
    // include status='processing' too — it gets a fresh pending job in
    // the new queue (the legacy worker isn't running anymore to
    // complete it).
    let pending = conn.query_all(
        "SELECT collection, document_id, source_path, target_path, format, \
                quality, url_column, url_value \
         FROM _crap_image_queue \
         WHERE status IN ('pending', 'processing', 'failed')",
        &[],
    )?;

    let mut drained = 0u64;
    for row in &pending {
        let data = ImageConvertJobData {
            collection: row.text_at(0).unwrap_or_default().to_string(),
            document_id: row.text_at(1).unwrap_or_default().to_string(),
            source_path: row.text_at(2).unwrap_or_default().to_string(),
            target_path: row.text_at(3).unwrap_or_default().to_string(),
            format: row.text_at(4).unwrap_or_default().to_string(),
            quality: u8::try_from(row.i64_at(5).unwrap_or(80)).unwrap_or(80),
            url_column: row.text_at(6).unwrap_or_default().to_string(),
            url_value: row.text_at(7).unwrap_or_default().to_string(),
        };
        let data_json = serde_json::to_string(&data)
            .context("Failed to serialize image-convert job during legacy drain")?;

        job_query::insert_job(
            conn,
            SYSTEM_IMAGE_CONVERT_JOB,
            &data_json,
            ScheduledBy::System,
            // Drain migration doesn't have JobsConfig in scope; use
            // the framework fallback. Operators tuning
            // `[jobs.queues.images] retries` only affect NEW jobs;
            // drained legacy entries get the baseline.
            FALLBACK_MAX_ATTEMPTS,
            IMAGE_CONVERT_QUEUE,
            0,
        )
        .context("Failed to insert drained image-convert job")?;

        drained += 1;
    }

    conn.execute_batch_ddl("DROP TABLE IF EXISTS _crap_image_queue")
        .context("Failed to drop legacy _crap_image_queue table")?;

    if drained > 0 {
        tracing::info!(
            "Drained {} legacy image-queue entries into _crap_jobs (table dropped)",
            drained
        );
    }

    Ok(())
}

#[cfg(test)]
#[allow(clippy::missing_panics_doc)]
mod tests {
    use super::*;
    use crate::{
        config::CrapConfig,
        core::CollectionDefinition,
        db::{DbValue, InMemoryConn, pool},
    };

    /// Only an orphan pointing at a rebuilt table is the sync's doing; one in
    /// an unrelated table predates the boot and must not block it.
    #[test]
    fn the_dangling_reference_check_is_scoped_to_the_rebuilt_tables() {
        let c = InMemoryConn::open();
        // Enforcement off to plant the orphan; `foreign_key_check` reports it
        // regardless of the pragma.
        c.setup(
            "PRAGMA foreign_keys = OFF; \
             CREATE TABLE p (id TEXT PRIMARY KEY); \
             CREATE TABLE p_tags (id TEXT, p_id TEXT REFERENCES p(id)); \
             INSERT INTO p_tags VALUES ('x', 'missing');",
        );

        assert_no_dangling_references(&c, &["other".to_string()])
            .expect("an orphan under a table that was not rebuilt is ignored");

        let err = assert_no_dangling_references(&c, &["p".to_string()])
            .expect_err("an orphan under the rebuilt table fails the sync");
        assert!(err.to_string().contains("p_tags"), "{err}");
    }

    /// Regression: an alpha.8 `_crap_jobs` table (without `priority` or
    /// `unique_key` columns) must upgrade cleanly via `create_jobs_table`.
    /// The bug this guards: an earlier alpha.9 build co-located the new
    /// indexes inside the CREATE TABLE batch. On upgrade,
    /// `CREATE TABLE IF NOT EXISTS` was a no-op against the existing
    /// table missing the new columns, and the subsequent
    /// `CREATE INDEX ... ON _crap_jobs(priority ...)` crashed with
    /// `no such column: priority` before the ALTER could run.
    #[test]
    fn create_jobs_table_upgrades_alpha8_schema() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = CrapConfig::default();
        let p = pool::create_pool(dir.path(), &config).unwrap();
        let conn = p.get().unwrap();

        // Simulate the alpha.8 `_crap_jobs` schema — no priority,
        // no unique_key, no retry_after. (Pre-alpha.3 didn't have
        // retry_after either; this covers older versions too.)
        conn.execute_batch(
            "CREATE TABLE _crap_jobs (
                id TEXT PRIMARY KEY,
                slug TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'pending',
                queue TEXT NOT NULL DEFAULT 'default',
                data TEXT DEFAULT '{}',
                result TEXT,
                error TEXT,
                attempt INTEGER NOT NULL DEFAULT 0,
                max_attempts INTEGER NOT NULL DEFAULT 1,
                scheduled_by TEXT,
                created_at TEXT DEFAULT (datetime('now')),
                started_at TEXT,
                completed_at TEXT,
                heartbeat_at TEXT
            );",
        )
        .unwrap();

        // Insert a row representative of alpha.8 data to make sure
        // the ALTERs don't corrupt anything.
        conn.execute(
            "INSERT INTO _crap_jobs (id, slug, status, queue, data, max_attempts, scheduled_by) \
             VALUES ('legacy-1', 'cleanup', 'pending', 'default', '{}', 1, 'cron')",
            &[],
        )
        .unwrap();

        // Upgrade path.
        create_jobs_table(&conn, "TEXT DEFAULT (datetime('now'))", "TEXT")
            .expect("create_jobs_table must upgrade alpha.8 schema cleanly");

        // All new columns present.
        let cols = conn.get_table_columns("_crap_jobs").unwrap();
        for col in ["priority", "unique_key", "retry_after"] {
            assert!(
                cols.contains(col),
                "expected `{col}` column after upgrade; got {cols:?}"
            );
        }

        // Legacy row preserved.
        let row = conn
            .query_one(
                "SELECT slug, priority, unique_key FROM _crap_jobs WHERE id = ?1",
                &[DbValue::Text("legacy-1".to_string())],
            )
            .unwrap()
            .expect("legacy row should still exist");
        assert_eq!(row.opt_text_at(0).as_deref(), Some("cleanup"));
        assert_eq!(row.i64_at(1), Some(0), "priority should default to 0");
        assert!(row.opt_text_at(2).is_none(), "unique_key should be NULL");

        // The new indexes exist (idempotent re-run is a no-op).
        create_jobs_table(&conn, "TEXT DEFAULT (datetime('now'))", "TEXT")
            .expect("second create_jobs_table call must be idempotent");
    }

    /// `migrate fresh` drops and recreates in one transaction: a recreate that
    /// fails (here on an identifier Postgres could not hold) leaves every table
    /// and row as it was — no half-dropped schema.
    #[test]
    fn a_failing_recreate_leaves_the_database_intact() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = pool::create_pool(dir.path(), &CrapConfig::default()).unwrap();
        let locale_config = LocaleConfig::default();

        let mut registry = Registry::new();
        registry.register_collection(CollectionDefinition::new("posts"));
        sync_all(&p, &registry, &locale_config).expect("sync");

        p.get()
            .unwrap()
            .execute("INSERT INTO posts (id) VALUES ('p1')", &[])
            .unwrap();

        let mut broken = Registry::new();
        broken.register_collection(CollectionDefinition::new("x".repeat(70).as_str()));
        recreate_all(&p, &broken, &locale_config).expect_err("the long slug fails the sync");

        let conn = p.get().unwrap();
        assert!(
            conn.query_one("SELECT id FROM posts WHERE id = 'p1'", &[])
                .unwrap()
                .is_some(),
            "the failed recreate must not have dropped anything"
        );

        drop(conn);
        recreate_all(&p, &registry, &locale_config).expect("recreate");

        let conn = p.get().unwrap();
        assert!(
            conn.query_one("SELECT id FROM posts WHERE id = 'p1'", &[])
                .unwrap()
                .is_none(),
            "a recreate empties the tables"
        );
    }

    /// Regression: the whole-database `ref_count_backfilled` flag was replaced
    /// by per-slug gates but never removed, so every database upgraded from a
    /// release that wrote it kept a key naming a pass nothing reads. Sync
    /// removes every retired gate, and leaves the live ones alone.
    #[test]
    fn sync_removes_retired_meta_keys() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = CrapConfig::default();
        let p = pool::create_pool(dir.path(), &config).unwrap();
        let registry = Registry::new();
        let locale_config = LocaleConfig::default();

        sync_all(&p, &registry, &locale_config).expect("first sync");

        let conn = p.get().unwrap();
        for key in RETIRED_META_KEYS {
            meta::upsert(&conn, key, "1").unwrap();
        }
        meta::upsert(&conn, "ref_count_backfilled:posts", "2").unwrap();
        drop(conn);

        sync_all(&p, &registry, &locale_config).expect("second sync");

        let conn = p.get().unwrap();
        for key in RETIRED_META_KEYS {
            assert_eq!(meta::get(&conn, key).unwrap(), None, "{key}");
        }
        assert_eq!(
            meta::get(&conn, "ref_count_backfilled:posts")
                .unwrap()
                .as_deref(),
            Some("2"),
            "a per-slug gate must survive"
        );
    }
}
