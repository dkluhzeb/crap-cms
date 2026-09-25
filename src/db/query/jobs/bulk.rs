//! Bulk operations: cancel pending, purge old, retry failed.

use std::fmt::Write as _;

use anyhow::{Context as _, Result};

use crate::db::{DbConnection, DbValue};

/// Cancel ONE pending run by id. Only a `pending` row can be cancelled —
/// a claimed/running job cannot be stopped mid-flight. Returns whether a
/// row was removed.
///
/// # Errors
///
/// Returns a backend error if the DELETE fails.
pub fn cancel_pending_job(conn: &dyn DbConnection, id: &str) -> Result<bool> {
    let affected = conn.execute(
        &format!(
            "DELETE FROM _crap_jobs WHERE status = 'pending' AND id = {}",
            conn.placeholder(1)
        ),
        &[DbValue::Text(id.to_string())],
    )?;

    Ok(affected > 0)
}

/// Cancel all pending jobs, optionally filtered by slug. Returns how many
/// rows were removed.
///
/// # Errors
///
/// Returns a backend error if the DELETE fails.
pub fn cancel_pending_jobs(conn: &dyn DbConnection, slug: Option<&str>) -> Result<i64> {
    let affected = if let Some(slug) = slug {
        conn.execute(
            &format!(
                "DELETE FROM _crap_jobs WHERE status = 'pending' AND slug = {}",
                conn.placeholder(1)
            ),
            &[DbValue::Text(slug.to_string())],
        )?
    } else {
        conn.execute("DELETE FROM _crap_jobs WHERE status = 'pending'", &[])?
    };

    i64::try_from(affected).context("cancelled count exceeds i64::MAX")
}

/// Put failed runs of job `slug` back to pending at `priority`, fresh: no
/// error, no attempts, no timestamps of the failed run. Only the run `id` when
/// set, every failed run of the slug otherwise. Returns how many were reset.
///
/// # Errors
///
/// Returns a backend error if the UPDATE fails.
pub fn retry_failed_jobs(
    conn: &dyn DbConnection,
    slug: &str,
    id: Option<&str>,
    priority: i32,
) -> Result<usize> {
    let mut params = vec![
        DbValue::Integer(i64::from(priority)),
        DbValue::Text(slug.to_string()),
    ];

    let id_sql = match id {
        Some(id) => {
            params.push(DbValue::Text(id.to_string()));

            format!(" AND id = {}", conn.placeholder(3))
        }
        None => String::new(),
    };

    let sql = format!(
        "UPDATE _crap_jobs SET status = 'pending', error = NULL, attempt = 0, \
         completed_at = NULL, started_at = NULL, retry_after = NULL, priority = {} \
         WHERE slug = {} AND status = 'failed'{id_sql}",
        conn.placeholder(1),
        conn.placeholder(2)
    );

    conn.execute(&sql, &params)
}

/// Delete completed/failed/stale job runs older than the given threshold.
/// Returns the number of rows deleted.
///
/// # Errors
///
/// Returns a backend error if the DELETE fails.
pub fn purge_old_jobs(conn: &dyn DbConnection, older_than_secs: u64) -> Result<i64> {
    purge_finished_runs(conn, older_than_secs, None)
}

/// [`purge_old_jobs`] restricted to the runs of one job `slug`.
///
/// # Errors
///
/// Returns a backend error if the DELETE fails.
pub fn purge_old_jobs_for_slug(
    conn: &dyn DbConnection,
    slug: &str,
    older_than_secs: u64,
) -> Result<i64> {
    purge_finished_runs(conn, older_than_secs, Some(slug))
}

fn purge_finished_runs(
    conn: &dyn DbConnection,
    older_than_secs: u64,
    slug: Option<&str>,
) -> Result<i64> {
    let older = i64::try_from(older_than_secs)
        .context("older_than_secs exceeds the SQL TIMESTAMP arithmetic range")?;
    let (offset_sql, offset_param) = conn.date_offset_expr(older, 1);
    let mut params = vec![offset_param];

    let slug_clause = match slug {
        Some(slug) => {
            params.push(DbValue::Text(slug.to_string()));
            format!(" AND slug = {}", conn.placeholder(2))
        }
        None => String::new(),
    };

    let deleted = i64::try_from(conn.execute(
        &format!(
            // Retention is measured from when the run FINISHED, not when it was
            // queued: a run held pending by a long `delay` (or accumulated
            // backoff) past the retention window would otherwise be purged the
            // instant it completes, before the queuer can poll its result.
            // `stale` rows without a `completed_at` fall back to `created_at`.
            "DELETE FROM _crap_jobs
             WHERE status IN ('completed', 'failed', 'stale')
               AND COALESCE(completed_at, created_at) < {offset_sql}{slug_clause}"
        ),
        &params,
    )?)
    .context("delete count exceeds i64::MAX")?;

    Ok(deleted)
}

/// Delete pending/failed jobs of `slug` whose `data` matches ALL of the given
/// `LIKE` patterns. Used to drop superseded image-convert jobs when their
/// owning document is deleted. Patterns are bound as parameters.
///
/// # Errors
///
/// Returns a backend error if the DELETE fails.
pub fn delete_pending_failed_jobs_matching(
    conn: &dyn DbConnection,
    slug: &str,
    data_patterns: &[String],
) -> Result<()> {
    let mut sql = format!(
        "DELETE FROM _crap_jobs WHERE slug = {} AND status IN ('pending', 'failed')",
        conn.placeholder(1)
    );
    let mut params: Vec<DbValue> = vec![DbValue::Text(slug.to_string())];

    for pat in data_patterns {
        params.push(DbValue::Text(pat.clone()));
        // ESCAPE '\' so a caller can escape LIKE wildcards (`\_`, `\%`) in the
        // literal parts of a pattern (see `like_escape`) while keeping its own
        // surrounding `%` as real wildcards.
        let _ = write!(
            sql,
            " AND data LIKE {} ESCAPE '\\'",
            conn.placeholder(params.len())
        );
    }

    conn.execute(&sql, &params)
        .with_context(|| format!("Failed to delete pending/failed jobs for slug '{slug}'"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::upload::{SYSTEM_IMAGE_CONVERT_JOB, delete_image_jobs_for_document};
    use crate::core::{JobStatus, ScheduledBy};
    use crate::db::query::jobs::test_helpers::setup_db;
    use crate::db::query::jobs::{insert_job, list_job_runs};

    #[test]
    fn test_purge_old_jobs() {
        let (_dir, conn) = setup_db();
        // Insert a completed job with old timestamp
        conn.execute(
            "INSERT INTO _crap_jobs (id, slug, status, created_at) VALUES ('old1', 'test', 'completed', strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-30 days'))",
            &[],
        ).unwrap();
        conn.execute(
            "INSERT INTO _crap_jobs (id, slug, status, created_at) VALUES ('new1', 'test', 'completed', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
            &[],
        ).unwrap();

        let deleted = purge_old_jobs(&conn, 86400 * 7).unwrap(); // 7 days
        assert_eq!(deleted, 1);

        let remaining = list_job_runs(&conn, None, None, 100, 0).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, "new1");
    }

    /// A retry resets only failed runs of the slug — one by id, or all — and
    /// leaves every other run alone.
    #[test]
    fn retry_failed_jobs_resets_only_failed_runs_of_the_slug() {
        let (_dir, conn) = setup_db();
        conn.execute_batch(
            "INSERT INTO _crap_jobs (id, slug, status, error, attempt) VALUES
                ('f1', 'img', 'failed', 'boom', 3),
                ('f2', 'img', 'failed', 'boom', 3),
                ('ok', 'img', 'completed', NULL, 1),
                ('other', 'mail', 'failed', 'boom', 3);",
        )
        .unwrap();

        assert_eq!(retry_failed_jobs(&conn, "img", Some("f1"), 5).unwrap(), 1);
        assert_eq!(retry_failed_jobs(&conn, "img", Some("ok"), 0).unwrap(), 0);
        assert_eq!(retry_failed_jobs(&conn, "img", None, 0).unwrap(), 1);

        let status = |id: &str| {
            conn.query_one(
                "SELECT status, error, attempt, priority FROM _crap_jobs WHERE id = ?1",
                &[DbValue::Text(id.to_string())],
            )
            .unwrap()
            .unwrap()
        };

        let f1 = status("f1");
        assert_eq!(f1.get_string("status").unwrap(), "pending");
        assert!(f1.get_opt_string("error").unwrap().is_none());
        assert_eq!(f1.get_i64("attempt").unwrap(), 0);
        assert_eq!(f1.get_i64("priority").unwrap(), 5);

        assert_eq!(status("f2").get_string("status").unwrap(), "pending");
        assert_eq!(status("ok").get_string("status").unwrap(), "completed");
        assert_eq!(status("other").get_string("status").unwrap(), "failed");
    }

    /// Regression: `cancel_pending_jobs` used `name` instead of `slug` column.
    #[test]
    fn test_cancel_pending_jobs_by_slug() {
        let (_dir, conn) = setup_db();

        insert_job(&conn, "cleanup", "{}", ScheduledBy::Cli, 1, "default", 0).unwrap();
        insert_job(&conn, "notify", "{}", ScheduledBy::Cli, 1, "default", 0).unwrap();

        // Cancel only "cleanup" pending jobs
        let deleted = cancel_pending_jobs(&conn, Some("cleanup")).unwrap();
        assert_eq!(deleted, 1, "should cancel exactly one job");

        // "notify" should still be pending
        let runs = list_job_runs(&conn, Some("notify"), None, 10, 0).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, JobStatus::Pending);

        // Cancel all remaining pending
        let deleted = cancel_pending_jobs(&conn, None).unwrap();
        assert_eq!(deleted, 1, "should cancel the remaining pending job");
    }

    #[test]
    fn delete_pending_failed_jobs_matching_filters_by_slug_and_data() {
        let (_dir, conn) = setup_db();

        insert_job(
            &conn,
            "img",
            r#"{"collection":"media","document_id":"d1"}"#,
            ScheduledBy::System,
            1,
            "default",
            0,
        )
        .unwrap();
        insert_job(
            &conn,
            "img",
            r#"{"collection":"media","document_id":"d2"}"#,
            ScheduledBy::System,
            1,
            "default",
            0,
        )
        .unwrap();
        // Same payload but a different slug — must be left alone.
        insert_job(
            &conn,
            "other",
            r#"{"collection":"media","document_id":"d1"}"#,
            ScheduledBy::System,
            1,
            "default",
            0,
        )
        .unwrap();

        let patterns = vec![
            "%\"collection\":\"media\"%".to_string(),
            "%\"document_id\":\"d1\"%".to_string(),
        ];
        delete_pending_failed_jobs_matching(&conn, "img", &patterns).unwrap();

        let remaining = list_job_runs(&conn, None, None, 100, 0).unwrap();
        assert_eq!(remaining.len(), 2, "only img+d1 should be deleted");
        assert!(
            !remaining
                .iter()
                .any(|r| r.slug == "img" && r.data.contains("\"d1\"")),
            "the matching img/d1 job must be gone"
        );
        assert!(
            remaining
                .iter()
                .any(|r| r.slug == "img" && r.data.contains("\"d2\"")),
            "a non-matching data pattern (d2) must be kept"
        );
        assert!(
            remaining.iter().any(|r| r.slug == "other"),
            "a different slug must be kept even with matching data"
        );
    }

    /// Regression: a document id contains `_` (a single-char LIKE wildcard),
    /// so cleaning up `abc_def`'s image jobs must NOT also delete a sibling
    /// `abcXdef`'s still-live conversions. Without `like_escape` + `ESCAPE`,
    /// the `_` matched `X` and over-deleted.
    #[test]
    fn delete_image_jobs_for_document_escapes_underscore_id() {
        let (_dir, conn) = setup_db();

        insert_job(
            &conn,
            SYSTEM_IMAGE_CONVERT_JOB,
            r#"{"collection":"media","document_id":"abc_def"}"#,
            ScheduledBy::System,
            1,
            "default",
            0,
        )
        .unwrap();
        insert_job(
            &conn,
            SYSTEM_IMAGE_CONVERT_JOB,
            r#"{"collection":"media","document_id":"abcXdef"}"#,
            ScheduledBy::System,
            1,
            "default",
            0,
        )
        .unwrap();

        delete_image_jobs_for_document(&conn, "media", "abc_def").unwrap();

        let remaining = list_job_runs(&conn, None, None, 100, 0).unwrap();
        assert_eq!(
            remaining.len(),
            1,
            "only the exact-id job is deleted; the `_`-wildcard must not over-match the sibling"
        );
        assert!(
            remaining[0].data.contains("abcXdef"),
            "the surviving job must be the sibling `abcXdef`, got: {}",
            remaining[0].data
        );
    }

    /// The slug-scoped purge measures age from completion and leaves other
    /// job types alone.
    #[test]
    fn purge_old_jobs_for_slug_measures_from_completion_and_keeps_other_slugs() {
        let (_dir, conn) = setup_db();
        let old = "strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-30 days')";
        let now = "strftime('%Y-%m-%dT%H:%M:%fZ', 'now')";

        for (id, slug, created, completed) in [
            ("finished-long-ago", "img", old, old),
            ("finished-just-now", "img", old, now),
            ("other-type", "other", old, old),
        ] {
            conn.execute(
                &format!(
                    "INSERT INTO _crap_jobs (id, slug, status, created_at, completed_at) \
                     VALUES ('{id}', '{slug}', 'completed', {created}, {completed})"
                ),
                &[],
            )
            .unwrap();
        }

        let deleted = purge_old_jobs_for_slug(&conn, "img", 86_400 * 7).unwrap();
        assert_eq!(deleted, 1);

        let mut remaining: Vec<String> = list_job_runs(&conn, None, None, 100, 0)
            .unwrap()
            .into_iter()
            .map(|r| r.id)
            .collect();
        remaining.sort();
        assert_eq!(remaining, ["finished-just-now", "other-type"]);
    }
}
