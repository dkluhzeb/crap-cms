//! One bounded batch of the soft-delete retention purge.

use std::collections::HashSet;

use anyhow::{Error, Result};
use tracing::{debug, info, warn};

use crate::{
    config::LocaleConfig,
    core::{CollectionDefinition, Registry},
    db::{DbConnection, DbValue, LocaleContext, query},
    service::{PurgeEvents, owned_file_keys},
};

use super::{
    period::parse_retention_seconds,
    run::{Position, PurgeBatch, Resume, RetentionPurge},
};

/// Purge soft-deleted documents past their retention period — one bounded
/// batch of the transactional DB half. For each collection with
/// `soft_delete` + `soft_delete_retention`, hard-delete the trashed documents
/// older than the threshold that nothing references, examining at most the
/// run's batch size of candidates and advancing `run` past them.
///
/// The returned batch holds the file keys of the deleted uploads, which the
/// CALLER must delete **after committing** — so a crash/rollback leaves
/// orphaned files (safe) rather than DB rows pointing at deleted files
/// (unsafe) — and each purged document's delete event (when the run
/// captures), which the caller publishes after committing too. Call again
/// with the same `run`, in a fresh transaction, until [`RetentionPurge::is_done`].
///
/// `conn` MUST be an IMMEDIATE transaction: the per-doc locked ref-count check
/// → purge sequence relies on it being atomic (`SQLite`
/// serializes writers; Postgres holds the `FOR UPDATE` lock until commit) so a
/// concurrent create can't increment a to-be-purged doc's ref count between the
/// check and the delete and leave a dangling reference.
///
/// # Errors
///
/// Returns an error if a candidate scan or a document's purge fails. A
/// failed document is set aside first (see [`RetentionPurge`]); `run` does not
/// advance, so the caller rolls back and — when
/// [`RetentionPurge::retry_after_failure`] says so — runs the batch again.
pub fn purge_soft_deleted(
    conn: &dyn DbConnection,
    registry: &Registry,
    locale_config: &LocaleConfig,
    run: &mut RetentionPurge,
) -> Result<PurgeBatch> {
    let mut batch = PurgeBatch {
        purged: 0,
        files: Vec::new(),
        events: PurgeEvents::new(run.capture),
    };
    let mut budget = run.batch_size;

    for (slug, def, retention_seconds) in retention_collections(registry) {
        let after = match run.resume_in(slug) {
            Resume::Skip => continue,
            Resume::FromStart => None,
            Resume::After(id) => Some(id),
        };

        let input = PurgeCollectionInput {
            conn,
            def,
            retention_seconds,
            locale_config,
            after: after.as_deref(),
            limit: budget,
            failed: run.failed.rows.get(slug),
        };

        let scan = match purge_collection(&input, &mut batch) {
            Ok(scan) => scan,
            Err(PurgeFailure::Scan(e)) => return Err(e),
            Err(PurgeFailure::Row { id, error }) => {
                run.set_aside(slug, &id, &error);

                return Err(error.context(format!("purge {slug}/{id}")));
            }
        };

        if let Some(last_id) = scan.last_id.filter(|_| scan.examined == budget) {
            run.position = Position::After {
                slug: slug.to_string(),
                id: last_id,
            };

            return Ok(batch);
        }

        budget -= scan.examined;
    }

    run.position = Position::Done;

    Ok(batch)
}

/// The collections with a valid retention period, in slug order, with that
/// period in seconds.
fn retention_collections(registry: &Registry) -> Vec<(&str, &CollectionDefinition, i64)> {
    let mut collections: Vec<_> = registry
        .collections
        .iter()
        .filter(|(_, def)| def.soft_delete)
        .filter_map(|(slug, def)| {
            let retention = def.soft_delete_retention.as_ref()?;

            let Some(seconds) = parse_retention_seconds(retention) else {
                warn!("Invalid soft_delete_retention '{retention}' for collection '{slug}'");

                return None;
            };

            Some((&**slug, &**def, seconds))
        })
        .collect();

    collections.sort_by_key(|(slug, ..)| *slug);

    collections
}

/// Borrowed inputs of one collection's share of a retention-purge batch.
struct PurgeCollectionInput<'a> {
    conn: &'a dyn DbConnection,
    def: &'a CollectionDefinition,
    retention_seconds: i64,
    locale_config: &'a LocaleConfig,
    /// Resume after this id (`None`: from the first candidate).
    after: Option<&'a str>,
    /// At most this many candidates are examined.
    limit: usize,
    /// Documents the run has set aside after a failed purge.
    failed: Option<&'a HashSet<String>>,
}

/// How far one collection's scan got.
struct CollectionScan {
    /// Candidates examined, purged or not.
    examined: usize,
    /// The last candidate examined — where the next batch resumes.
    last_id: Option<String>,
}

/// Why one collection's share of a batch failed.
enum PurgeFailure {
    /// The candidate scan itself failed.
    Scan(Error),
    /// Purging document `id` failed.
    Row { id: String, error: Error },
}

/// Purge a collection's expired soft-deleted documents, at most `limit`
/// candidates after `after`, skipping any the run has set aside.
fn purge_collection(
    p: &PurgeCollectionInput<'_>,
    batch: &mut PurgeBatch,
) -> Result<CollectionScan, PurgeFailure> {
    let ids = expired_candidates(p).map_err(PurgeFailure::Scan)?;

    // The row reads need the locale context: a collection with localized
    // fields has no bare columns to select.
    let locale_ctx = LocaleContext::default_for(p.locale_config);
    let purged_before = batch.purged;

    for id in &ids {
        if p.failed.is_some_and(|failed| failed.contains(id)) {
            continue;
        }

        purge_candidate(p, id, locale_ctx.as_ref(), batch).map_err(|error| PurgeFailure::Row {
            id: id.clone(),
            error,
        })?;
    }

    let purged = batch.purged - purged_before;

    if purged > 0 {
        info!(
            "Purged {purged} expired soft-deleted doc(s) from '{}'",
            p.def.slug
        );
    }

    Ok(CollectionScan {
        examined: ids.len(),
        last_id: ids.last().cloned(),
    })
}

/// The ids of the collection's documents trashed past retention, after
/// `p.after` in id order, at most `p.limit` of them. Read without a lock —
/// [`purge_candidate`] re-checks each under one.
fn expired_candidates(p: &PurgeCollectionInput<'_>) -> Result<Vec<String>> {
    let (offset_sql, offset_param) = p.conn.date_offset_expr(p.retention_seconds, 1);
    let mut params = vec![offset_param];

    let after_sql = match p.after {
        Some(after) => {
            params.push(DbValue::Text(after.to_string()));

            format!(" AND id > {}", p.conn.placeholder(2))
        }
        None => String::new(),
    };

    let sql = format!(
        "SELECT id FROM \"{}\" WHERE _deleted_at IS NOT NULL AND _deleted_at < {offset_sql}\
         {after_sql} ORDER BY id LIMIT {}",
        p.def.slug, p.limit
    );

    let rows = p.conn.query_all(&sql, &params)?;

    Ok(rows
        .iter()
        .filter_map(|row| match row.get_value(0) {
            Some(DbValue::Text(id)) => Some(id.clone()),
            _ => None,
        })
        .collect())
}

/// Purge one candidate if it is still trashed past retention and nothing
/// references it.
///
/// Every read of the row — the storage keys its upload owns (its row's AND
/// its version snapshots') and its delete event — finishes before anything is
/// written, and a failure of any of them fails the document rather than
/// purging it without its files or its event. The keys go to the caller only
/// once the row is gone, for deletion after the commit: a crash between DB
/// delete and file delete leaves orphaned files (safe), never DB records
/// pointing to deleted files (unsafe).
fn purge_candidate(
    p: &PurgeCollectionInput<'_>,
    id: &str,
    locale_ctx: Option<&LocaleContext>,
    batch: &mut PurgeBatch,
) -> Result<()> {
    if !is_purgeable(p, id)? {
        return Ok(());
    }

    let keys = owned_file_keys(p.conn, p.def, id, locale_ctx)?;
    let captured = batch.events.capture(p.conn, p.def, id, p.locale_config)?;

    // The hard delete with every cleanup it needs (ref counts, FTS entry,
    // queued image conversions), shared with the service and CLI purges.
    if batch
        .events
        .complete(p.conn, p.def, captured, p.locale_config)?
    {
        batch.purged += 1;
        batch.files.extend(keys);
    }

    Ok(())
}

/// Lock the row and re-check that it is still trashed past retention and
/// unreferenced: the candidates were read without a lock, and a restore may
/// have committed since. The same lock keeps a concurrent create from
/// incrementing the ref count between this check and the DELETE (Postgres
/// only; `SQLite` serializes via IMMEDIATE).
fn is_purgeable(p: &PurgeCollectionInput<'_>, id: &str) -> Result<bool> {
    let slug = &p.def.slug;

    let Some(ref_count) =
        query::ref_count::get_purgeable_ref_count_locked(p.conn, slug, id, p.retention_seconds)?
    else {
        debug!("Skipping purge of {slug}/{id}: no longer trashed past retention");

        return Ok(false);
    };

    // Skip documents that are still referenced -- protect referential integrity.
    if ref_count > 0 {
        debug!("Skipping purge of {slug}/{id}: referenced by {ref_count} document(s)");

        return Ok(false);
    }

    Ok(true)
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::{
        core::{
            FieldDefinition, FieldType,
            job::JobStatus,
            upload::{self, CollectionUpload, SYSTEM_IMAGE_CONVERT_JOB},
        },
        db::query::jobs as job_query,
        scheduler::runner::{
            retention::run::MAX_FAILED_ROWS_PER_COLLECTION,
            test_support::{convert_job, make_test_pool},
        },
    };

    // ── purge_soft_deleted ────────────────────────────────────────────────

    /// A soft-delete collection `slug` with a one-minute retention.
    fn retention_def(slug: &str) -> CollectionDefinition {
        let mut def = CollectionDefinition::new(slug);
        def.soft_delete = true;
        def.soft_delete_retention = Some("1m".to_string());
        def
    }

    /// A retention collection whose row reads fail: it declares an array
    /// field whose join table is never created. (A missing *column* would not
    /// do: `SQLite`'s double-quoted-string fallback turns `SELECT "gone_col"`
    /// into a string literal instead of an error.)
    fn unreadable_def(slug: &str) -> CollectionDefinition {
        let mut def = retention_def(slug);
        def.fields = vec![
            FieldDefinition::builder("shots", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("caption", FieldType::Text).build(),
                ])
                .build(),
        ];
        def
    }

    fn registry_of(defs: Vec<CollectionDefinition>) -> Arc<Registry> {
        let shared = Registry::shared();

        {
            let mut registry = shared.write().unwrap();

            for def in defs {
                registry.register_collection(def);
            }
        }

        Registry::snapshot(&shared)
    }

    /// Create table `slug` holding `rows` of `(id, trashed long ago?,
    /// ref count)`.
    fn trash_table(conn: &dyn DbConnection, slug: &str, rows: &[(&str, bool, i64)]) {
        let values: Vec<String> = rows
            .iter()
            .map(|(id, trashed, refs)| {
                let deleted_at = if *trashed {
                    "'2000-01-01T00:00:00Z'"
                } else {
                    "NULL"
                };

                format!("('{id}', {deleted_at}, {refs})")
            })
            .collect();

        conn.execute_batch(&format!(
            "CREATE TABLE \"{slug}\" (
                id TEXT PRIMARY KEY,
                _deleted_at TEXT,
                _ref_count INTEGER DEFAULT 0,
                created_at TEXT,
                updated_at TEXT
            );
            INSERT INTO \"{slug}\" (id, _deleted_at, _ref_count) VALUES {};",
            values.join(", ")
        ))
        .unwrap();
    }

    fn remaining_ids(conn: &dyn DbConnection, slug: &str) -> Vec<String> {
        conn.query_all(&format!("SELECT id FROM \"{slug}\" ORDER BY id"), &[])
            .unwrap()
            .iter()
            .filter_map(|row| match row.get_value(0) {
                Some(DbValue::Text(id)) => Some(id.clone()),
                _ => None,
            })
            .collect()
    }

    fn captured_ids(batch: &PurgeBatch) -> Vec<String> {
        batch
            .events
            .captured()
            .iter()
            .map(|(id, _)| (*id).to_string())
            .collect()
    }

    /// Regression: a failed upload-fields read during purge was silently
    /// swallowed (`let Ok(Some(doc)) = …`), so the row was hard-deleted
    /// anyway and its files on disk were orphaned with nothing left to find
    /// them by. A failed read fails the document: its batch is rolled back
    /// and re-run without it, and the row survives for the next run.
    #[test]
    fn a_row_whose_upload_files_cannot_be_read_is_set_aside() {
        let pool = make_test_pool();
        let conn = pool.get().unwrap();
        trash_table(&conn, "pmedia", &[("m1", true, 0)]);

        let mut def = unreadable_def("pmedia");
        def.upload = Some(CollectionUpload {
            enabled: true,
            ..Default::default()
        });
        let registry = registry_of(vec![def]);
        let locale = LocaleConfig::default();

        let mut run = RetentionPurge::new(false);
        assert!(purge_soft_deleted(&conn, &registry, &locale, &mut run).is_err());
        assert!(run.retry_after_failure(), "the failed row is set aside");

        let batch = purge_soft_deleted(&conn, &registry, &locale, &mut run).unwrap();

        assert_eq!(batch.purged, 0, "an unreadable row is never purged");
        assert!(batch.files.is_empty());
        assert!(run.is_done());
        assert_eq!(remaining_ids(&conn, "pmedia"), ["m1"]);
    }

    /// Regression: a row whose delete event could not be read failed the whole
    /// purge with it, every run, so one bad row stopped all retention purging
    /// for good. It is set aside instead — logged, left trashed, retried by
    /// the next run, never purged without its event — and the rest is purged.
    #[test]
    fn a_row_whose_delete_event_cannot_be_read_does_not_block_the_purge() {
        let pool = make_test_pool();
        let conn = pool.get().unwrap();
        trash_table(&conn, "broken", &[("b1", true, 0)]);
        trash_table(&conn, "pmedia", &[("m1", true, 0)]);

        let registry = registry_of(vec![unreadable_def("broken"), retention_def("pmedia")]);
        let locale = LocaleConfig::default();

        let mut run = RetentionPurge::new(true);
        assert!(purge_soft_deleted(&conn, &registry, &locale, &mut run).is_err());
        assert!(run.retry_after_failure());

        let batch = purge_soft_deleted(&conn, &registry, &locale, &mut run).unwrap();

        assert_eq!(batch.purged, 1);
        assert_eq!(captured_ids(&batch), ["m1"]);
        assert!(run.is_done());
        assert_eq!(remaining_ids(&conn, "broken"), ["b1"]);
        assert!(remaining_ids(&conn, "pmedia").is_empty());
    }

    /// A collection whose every row fails costs a bounded number of retries:
    /// past the limit the collection is set aside for the run, and the other
    /// collections are still purged.
    #[test]
    fn a_collection_failing_on_every_row_is_set_aside_after_the_limit() {
        let pool = make_test_pool();
        let conn = pool.get().unwrap();

        let ids: Vec<String> = (0..=MAX_FAILED_ROWS_PER_COLLECTION)
            .map(|n| format!("b{n:02}"))
            .collect();
        let rows: Vec<(&str, bool, i64)> = ids.iter().map(|id| (id.as_str(), true, 0)).collect();
        trash_table(&conn, "broken", &rows);
        trash_table(&conn, "pmedia", &[("m1", true, 0)]);

        let registry = registry_of(vec![unreadable_def("broken"), retention_def("pmedia")]);
        let locale = LocaleConfig::default();
        let mut run = RetentionPurge::new(true);

        let mut failures = 0;
        let batch = loop {
            if let Ok(batch) = purge_soft_deleted(&conn, &registry, &locale, &mut run) {
                break batch;
            }

            assert!(run.retry_after_failure());
            failures += 1;
        };

        assert_eq!(failures, MAX_FAILED_ROWS_PER_COLLECTION);
        assert_eq!(batch.purged, 1);
        assert!(run.is_done());
        assert_eq!(remaining_ids(&conn, "broken").len(), ids.len());
    }

    /// A failure that is not one document's — the candidate scan itself —
    /// ends the run instead of retrying.
    #[test]
    fn a_failed_candidate_scan_is_not_retried() {
        let pool = make_test_pool();
        let conn = pool.get().unwrap();

        let registry = registry_of(vec![retention_def("missing")]);
        let mut run = RetentionPurge::new(false);

        let result = purge_soft_deleted(&conn, &registry, &LocaleConfig::default(), &mut run);

        assert!(result.is_err());
        assert!(!run.retry_after_failure());
    }

    /// Regression: the purge read and held every expired row in one
    /// transaction, however many there were. A batch examines at most the
    /// batch size of candidates, and the run resumes where it stopped.
    #[test]
    fn the_purge_runs_in_bounded_batches() {
        let pool = make_test_pool();
        let conn = pool.get().unwrap();
        trash_table(
            &conn,
            "pmedia",
            &[
                ("p1", true, 0),
                ("p2", true, 0),
                ("p3", true, 0),
                ("p4", true, 0),
                ("p5", true, 0),
                ("p6", false, 0),
            ],
        );

        let registry = registry_of(vec![retention_def("pmedia")]);
        let locale = LocaleConfig::default();
        let mut run = RetentionPurge::new(true).with_batch_size(2);

        let mut batches = Vec::new();

        while !run.is_done() {
            let batch = purge_soft_deleted(&conn, &registry, &locale, &mut run).unwrap();
            batches.push(captured_ids(&batch));
        }

        assert_eq!(
            batches,
            [vec!["p1", "p2"], vec!["p3", "p4"], vec!["p5"]],
            "each batch captures only its own rows' events"
        );
        assert_eq!(remaining_ids(&conn, "pmedia"), ["p6"]);
    }

    /// Candidates a batch examines but does not purge (still referenced) move
    /// the run on, so they cannot starve the rows behind them, and a batch
    /// continues into the next collection with what is left of its budget.
    #[test]
    fn skipped_candidates_advance_the_run_across_collections() {
        let pool = make_test_pool();
        let conn = pool.get().unwrap();
        trash_table(
            &conn,
            "alpha",
            &[("a1", true, 1), ("a2", true, 1), ("a3", true, 0)],
        );
        trash_table(&conn, "beta", &[("b1", true, 0)]);

        let registry = registry_of(vec![retention_def("beta"), retention_def("alpha")]);
        let locale = LocaleConfig::default();
        let mut run = RetentionPurge::new(false).with_batch_size(2);

        let mut purged = Vec::new();

        while !run.is_done() {
            let batch = purge_soft_deleted(&conn, &registry, &locale, &mut run).unwrap();
            purged.push(batch.purged);
        }

        assert_eq!(purged, [0, 2, 0]);
        assert_eq!(remaining_ids(&conn, "alpha"), ["a1", "a2"]);
        assert!(remaining_ids(&conn, "beta").is_empty());
    }

    /// The retention purge removes a purged upload's queued image conversions,
    /// as the service hard delete and the CLI purge do.
    #[test]
    fn retention_purge_cancels_queued_image_conversions() {
        let pool = make_test_pool();
        let conn = pool.get().unwrap();
        trash_table(&conn, "pmedia", &[("m1", true, 0)]);
        upload::queue_image_conversion(&conn, &convert_job("pmedia", "m1"), 1).unwrap();

        let mut def = retention_def("pmedia");
        def.upload = Some(CollectionUpload {
            enabled: true,
            ..Default::default()
        });
        let registry = registry_of(vec![def]);

        let mut run = RetentionPurge::new(false);
        let batch =
            purge_soft_deleted(&conn, &registry, &LocaleConfig::default(), &mut run).unwrap();

        assert_eq!(batch.purged, 1);

        let pending = job_query::count_job_runs(
            &conn,
            Some(SYSTEM_IMAGE_CONVERT_JOB),
            Some(JobStatus::Pending),
        )
        .unwrap();
        assert_eq!(
            pending, 0,
            "the purged upload's conversion must be cancelled"
        );
    }

    /// Regression: the retention purge hard-deleted trashed documents without
    /// a delete event, so a trash-view subscriber never learned they were
    /// gone. Each purged row's event is captured — gated by the trash, the
    /// view the row was last in — for publishing after the commit.
    #[test]
    fn retention_purge_captures_each_purged_rows_delete_event() {
        let pool = make_test_pool();
        let conn = pool.get().unwrap();
        trash_table(&conn, "pmedia", &[("m1", true, 0), ("m2", false, 0)]);

        let registry = registry_of(vec![retention_def("pmedia")]);

        let mut run = RetentionPurge::new(true);
        let batch =
            purge_soft_deleted(&conn, &registry, &LocaleConfig::default(), &mut run).unwrap();

        assert_eq!(batch.purged, 1);

        let captured = batch.events.captured();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].0, "m1");
        assert!(captured[0].1.trashed, "a purged row is gated by the trash");
    }

    /// A purge without an event transport reads nothing for the events.
    #[test]
    fn a_purge_without_a_transport_captures_nothing() {
        let pool = make_test_pool();
        let conn = pool.get().unwrap();
        trash_table(&conn, "pmedia", &[("m1", true, 0), ("m2", false, 0)]);

        let registry = registry_of(vec![retention_def("pmedia")]);

        let mut run = RetentionPurge::new(false);
        let batch =
            purge_soft_deleted(&conn, &registry, &LocaleConfig::default(), &mut run).unwrap();

        assert_eq!(batch.purged, 1);
        assert!(batch.events.captured().is_empty());
    }
}
