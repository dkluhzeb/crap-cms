//! Soft-delete retention purge and its multi-node tick claim.

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use tracing::{debug, info, warn};

use crate::{
    config::LocaleConfig,
    core::{CollectionDefinition, Registry},
    db::{DbConnection, DbValue, LocaleContext, query, query::jobs as job_query},
    service::{owned_file_keys, purge_document},
};

/// Parse a retention duration string like "30d", "7d", "24h" into seconds.
/// Returns `None` if the string is not a valid duration.
pub(crate) fn parse_retention_seconds(s: &str) -> Option<i64> {
    let s = s.trim();

    if let Some(days) = s.strip_suffix('d') {
        days.parse::<i64>().ok().map(|d| d * 86400)
    } else if let Some(hours) = s.strip_suffix('h') {
        hours.parse::<i64>().ok().map(|h| h * 3600)
    } else if let Some(mins) = s.strip_suffix('m') {
        mins.parse::<i64>().ok().map(|m| m * 60)
    } else if let Some(secs) = s.strip_suffix('s') {
        secs.parse::<i64>().ok()
    } else {
        s.parse::<i64>().ok() // raw seconds
    }
}

/// Purge soft-deleted documents past their retention period — the
/// transactional DB half. For each collection with `soft_delete` +
/// `soft_delete_retention`, hard-delete the trashed documents older than the
/// threshold that nothing references. Returns the number of docs deleted and
/// the file keys of the deleted uploads, which the CALLER must delete **after
/// committing** — so a crash/rollback leaves orphaned files (safe) rather than
/// DB rows pointing at deleted files (unsafe).
///
/// `conn` MUST be an IMMEDIATE transaction: the per-doc locked ref-count check
/// → [`purge_document`] sequence relies on it being atomic (`SQLite`
/// serializes writers; Postgres holds the `FOR UPDATE` lock until commit) so a
/// concurrent create can't increment a to-be-purged doc's ref count between the
/// check and the delete and leave a dangling reference.
///
/// # Errors
///
/// Returns an error if a candidate scan, a locked re-check, or a hard delete
/// fails.
pub fn purge_soft_deleted(
    conn: &dyn DbConnection,
    registry: &Registry,
    locale_config: &LocaleConfig,
) -> Result<(u64, Vec<String>)> {
    let mut total = 0u64;
    let mut keys_to_clean: Vec<String> = Vec::new();

    for (slug, def) in &registry.collections {
        if !def.soft_delete {
            continue;
        }

        let Some(ref retention) = def.soft_delete_retention else {
            continue;
        };

        let Some(seconds) = parse_retention_seconds(retention) else {
            warn!(
                "Invalid soft_delete_retention '{}' for collection '{}'",
                retention, slug
            );
            continue;
        };

        let (purged, keys) = purge_collection(&PurgeCollectionInput {
            conn,
            slug,
            def,
            retention_seconds: seconds,
            locale_config,
        })?;
        total += purged;
        keys_to_clean.extend(keys);
    }

    Ok((total, keys_to_clean))
}

/// Borrowed inputs of a single collection's retention purge.
struct PurgeCollectionInput<'a> {
    conn: &'a dyn DbConnection,
    slug: &'a str,
    def: &'a CollectionDefinition,
    retention_seconds: i64,
    locale_config: &'a LocaleConfig,
}

/// Purge expired soft-deleted documents from a single collection.
///
/// Collects the storage keys each upload owns — its row's AND its version
/// snapshots' — before deleting them, so the caller removes those files once
/// the DB deletes commit. A crash between DB delete and file delete leaves
/// orphaned files (safe), rather than orphaned DB records pointing to deleted
/// files (unsafe).
fn purge_collection(p: &PurgeCollectionInput<'_>) -> Result<(u64, Vec<String>)> {
    // Find docs past the retention threshold
    let (offset_sql, offset_param) = p.conn.date_offset_expr(p.retention_seconds, 1);
    let threshold_sql = format!(
        "SELECT id FROM \"{}\" WHERE _deleted_at IS NOT NULL \
         AND _deleted_at < {}",
        p.slug, offset_sql
    );
    let rows = p.conn.query_all(&threshold_sql, &[offset_param])?;

    let mut purged = 0u64;
    let mut upload_keys = Vec::new();
    // The upload row lookup needs the locale context: a collection with
    // localized fields has no bare columns to select.
    let locale_ctx = LocaleContext::default_for(p.locale_config);

    for row in &rows {
        let id = match row.get_value(0) {
            Some(DbValue::Text(s)) => s.clone(),
            _ => continue,
        };

        // Lock the row and re-check that it is still trashed past retention: the
        // candidates above were read without a lock, and a restore may have
        // committed since. The same lock keeps a concurrent create from
        // incrementing the ref count between this check and the DELETE
        // (Postgres only; SQLite serializes via IMMEDIATE).
        let Some(ref_count) = query::ref_count::get_purgeable_ref_count_locked(
            p.conn,
            p.slug,
            &id,
            p.retention_seconds,
        )?
        else {
            debug!(
                "Skipping purge of {}/{}: no longer trashed past retention",
                p.slug, id
            );
            continue;
        };

        // Skip documents that are still referenced -- protect referential integrity.
        if ref_count > 0 {
            debug!(
                "Skipping purge of {}/{}: referenced by {} document(s)",
                p.slug, id, ref_count
            );
            continue;
        }

        // Collect the storage keys the document owns BEFORE deleting it from
        // the DB; the caller deletes the actual files after committing the
        // transaction. A failed read must SKIP this row (not proceed):
        // deleting anyway would orphan the files on disk with nothing left to
        // find them by. This runs before the hard delete so a skipped row
        // leaves the targets' ref counts untouched for the retry on the next
        // purge tick.
        match owned_file_keys(p.conn, p.def, &id, locale_ctx.as_ref()) {
            Ok(keys) => upload_keys.extend(keys),
            Err(e) => {
                warn!(
                    "Skipping purge of {}/{}: failed to read upload files: {e}",
                    p.slug, id
                );
                continue;
            }
        }

        // The hard delete with every cleanup it needs (ref counts, FTS entry,
        // queued image conversions), shared with the service and CLI purges.
        if purge_document(p.conn, p.def, &id, p.locale_config)? {
            purged += 1;
        }
    }

    if purged > 0 {
        info!(
            "Purged {} expired soft-deleted doc(s) from '{}'",
            purged, p.slug
        );
    }

    Ok((purged, upload_keys))
}

/// Dedup slug used to claim the retention-purge cron tick via
/// `_crap_cron_fired`. Retention purge is a "pseudo cron" job — it runs on a
/// fixed interval from the scheduler loop rather than a user-defined cron
/// expression, but must still be deduped across instances in multi-node
/// deployments.
pub(super) const RETENTION_PURGE_SLUG: &str = "__retention_purge";

/// Attempt to claim the retention-purge tick for this instance/window.
///
/// Returns `true` iff this caller won the tick and should run the purge.
/// Uses the same `_crap_cron_fired` dedup table as user cron jobs.
/// `window_seconds` must match the scheduler's purge cadence so two instances
/// firing inside the same window still end up with exactly one winner.
pub(in crate::scheduler) fn claim_retention_purge_tick(
    conn: &dyn DbConnection,
    now: DateTime<Utc>,
    window_seconds: i64,
) -> Result<bool> {
    let fired_at = now.to_rfc3339();
    let window_start = (now - Duration::seconds(window_seconds)).to_rfc3339();

    job_query::try_claim_cron_window(conn, RETENTION_PURGE_SLUG, &fired_at, &window_start)
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use super::*;
    use crate::{
        core::{
            FieldDefinition, FieldType,
            job::JobStatus,
            upload::{self, CollectionUpload, SYSTEM_IMAGE_CONVERT_JOB},
        },
        scheduler::runner::test_support::{convert_job, make_test_pool},
    };

    // ── parse_retention_seconds ───────────────────────────────────────────

    #[test]
    fn parse_retention_days() {
        assert_eq!(parse_retention_seconds("30d"), Some(30 * 86400));
        assert_eq!(parse_retention_seconds("7d"), Some(7 * 86400));
        assert_eq!(parse_retention_seconds("1d"), Some(86400));
    }

    #[test]
    fn parse_retention_hours() {
        assert_eq!(parse_retention_seconds("24h"), Some(24 * 3600));
        assert_eq!(parse_retention_seconds("1h"), Some(3600));
    }

    #[test]
    fn parse_retention_minutes() {
        assert_eq!(parse_retention_seconds("30m"), Some(1800));
        assert_eq!(parse_retention_seconds("1m"), Some(60));
    }

    #[test]
    fn parse_retention_seconds_suffix() {
        assert_eq!(parse_retention_seconds("10s"), Some(10));
        assert_eq!(parse_retention_seconds("1s"), Some(1));
        assert_eq!(parse_retention_seconds("0s"), Some(0));
    }

    #[test]
    fn parse_retention_raw_seconds() {
        assert_eq!(parse_retention_seconds("3600"), Some(3600));
        assert_eq!(parse_retention_seconds("86400"), Some(86400));
    }

    #[test]
    fn parse_retention_invalid() {
        assert_eq!(parse_retention_seconds("abc"), None);
        assert_eq!(parse_retention_seconds(""), None);
        assert_eq!(parse_retention_seconds("d"), None);
    }

    #[test]
    fn parse_retention_with_whitespace() {
        assert_eq!(parse_retention_seconds(" 30d "), Some(30 * 86400));
        assert_eq!(parse_retention_seconds(" 3600 "), Some(3600));
    }

    // ── claim_retention_purge_tick ────────────────────────────────────────

    /// Two concurrent claims in the same window: only one wins. Locks in the
    /// retention-purge dedup so multi-node deployments don't run the purge N
    /// times per tick.
    #[test]
    fn retention_purge_claims_cron_tick_atomically() {
        let pool = make_test_pool();
        let conn = pool.get().unwrap();

        let now = Utc::now();
        let window_secs = 600; // 10 cron ticks of 60s

        // First call wins.
        let first =
            claim_retention_purge_tick(&conn, now, window_secs).expect("first claim must succeed");
        assert!(first, "first claim in a fresh window should win");

        // Second call immediately after, same window: must lose.
        let second = claim_retention_purge_tick(&conn, now, window_secs)
            .expect("second claim must succeed (returns Ok)");
        assert!(
            !second,
            "second claim inside the same window must return false"
        );

        // A call well past the window: must win again (next tick).
        let later = now + Duration::seconds(window_secs * 2);
        let third = claim_retention_purge_tick(&conn, later, window_secs)
            .expect("later claim must succeed");
        assert!(third, "a claim past the window should win again");
    }

    // ── purge_collection ──────────────────────────────────────────────────

    /// Regression: a failed upload-fields read during purge was silently
    /// swallowed (`let Ok(Some(doc)) = …`), so the row was hard-deleted
    /// anyway and its files on disk were orphaned with nothing left to find
    /// them by. A failed read must skip the row (retry on the next tick).
    ///
    /// The read is broken via a MISSING JOIN TABLE (the def declares an
    /// array field but `pmedia_shots` doesn't exist) — a missing *column*
    /// wouldn't work: `SQLite`'s double-quoted-string fallback turns
    /// `SELECT "gone_col"` into a string literal instead of an error.
    #[test]
    fn purge_skips_row_when_upload_fields_unreadable() {
        let pool = make_test_pool();

        let mut def = CollectionDefinition::new("pmedia");
        def.soft_delete = true;
        def.fields = vec![
            FieldDefinition::builder("alt", FieldType::Text).build(),
            FieldDefinition::builder("shots", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("caption", FieldType::Text).build(),
                ])
                .build(),
        ];
        def.upload = Some(CollectionUpload {
            enabled: true,
            ..Default::default()
        });

        let conn = pool.get().unwrap();
        conn.execute_batch(
            "CREATE TABLE pmedia (
                id TEXT PRIMARY KEY,
                alt TEXT,
                _deleted_at TEXT,
                _ref_count INTEGER DEFAULT 0
            );
            INSERT INTO pmedia (id, alt, _deleted_at)
                VALUES ('m1', 'old', '2000-01-01T00:00:00Z');",
        )
        .unwrap();

        let (purged, files) = purge_collection(&PurgeCollectionInput {
            conn: &conn,
            slug: "pmedia",
            def: &def,
            retention_seconds: 60,
            locale_config: &LocaleConfig::default(),
        })
        .unwrap();

        assert_eq!(purged, 0, "unreadable row must be skipped, not deleted");
        assert!(files.is_empty());

        let rows = conn
            .query_all("SELECT id FROM pmedia WHERE _deleted_at IS NOT NULL", &[])
            .unwrap();
        assert_eq!(
            rows.len(),
            1,
            "the row must survive so the next purge tick can retry"
        );
    }

    /// The retention purge removes a purged upload's queued image conversions,
    /// as the service hard delete and the CLI purge do.
    #[test]
    fn retention_purge_cancels_queued_image_conversions() {
        let pool = make_test_pool();

        let mut def = CollectionDefinition::new("pmedia");
        def.soft_delete = true;
        def.upload = Some(CollectionUpload {
            enabled: true,
            ..Default::default()
        });

        let conn = pool.get().unwrap();
        conn.execute_batch(
            "CREATE TABLE pmedia (
                id TEXT PRIMARY KEY,
                _deleted_at TEXT,
                _ref_count INTEGER DEFAULT 0,
                created_at TEXT,
                updated_at TEXT
            );
            INSERT INTO pmedia (id, _deleted_at)
                VALUES ('m1', '2000-01-01T00:00:00Z');",
        )
        .unwrap();
        upload::queue_image_conversion(&conn, &convert_job("pmedia", "m1"), 1).unwrap();

        let (purged, _) = purge_collection(&PurgeCollectionInput {
            conn: &conn,
            slug: "pmedia",
            def: &def,
            retention_seconds: 60,
            locale_config: &LocaleConfig::default(),
        })
        .unwrap();

        assert_eq!(purged, 1);

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
}
