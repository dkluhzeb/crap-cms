//! Atomic claim of pending jobs (sqlite + postgres backends).

use std::collections::HashMap;

use anyhow::{Result, anyhow};

use crate::core::{JobRun, JobStatus};
use crate::db::query::jobs::{count_running_per_queue, count_running_per_slug};
use crate::db::{DbConnection, DbRow, DbValue};

/// Atomically claim up to `limit` pending jobs by setting them to running.
/// Returns the claimed jobs. Respects per-slug + per-queue concurrency
/// limits, and honors priority ordering globally.
///
/// **Unified approach across backends:**
///
/// 1. Single `SELECT … ORDER BY priority DESC, created_at ASC LIMIT
///    N*2` across all rows. On Postgres the SELECT carries `FOR
///    UPDATE SKIP LOCKED` so other workers can't race on these rows;
///    on `SQLite` the caller's `IMMEDIATE` transaction serializes
///    writes the same way.
/// 2. Per-row check (per-slug + per-queue cap) in Rust.
/// 3. If the row passes, `UPDATE ... SET status='running' WHERE id=?
///    AND status='pending'`. The `AND status='pending'` is a belt-and-
///    suspenders guard against any drift; in practice the SELECT's
///    locking already guarantees we own the row.
///
/// Both backends get **strict global priority ordering** (the single
/// SELECT) and **strict per-slug / per-queue caps within a tick** (the
/// SELECT's locking holds candidate rows for our transaction).
///
/// `decay_secs` controls priority aging: `0` disables decay (pure
/// `priority DESC, created_at ASC` ordering — index-friendly fast path);
/// `>0` adds `(now - created_at) / decay_secs` to the effective priority
/// so old low-priority jobs age into being claimable.
///
/// `queue_concurrency` maps queue name → aggregate concurrent cap.
/// A queue absent from the map is unconstrained (only the global
/// `max_concurrent` and per-slug `job_concurrency` apply). Per-queue
/// caps are enforced cluster-wide via DB-sourced running counts.
///
/// # Errors
///
/// Returns a backend error if the claim query/update fails.
pub fn claim_pending_jobs(
    conn: &dyn DbConnection,
    limit: usize,
    job_concurrency: &HashMap<String, u32>,
    queue_concurrency: &HashMap<String, u32>,
    decay_secs: u64,
) -> Result<Vec<JobRun>> {
    let now = conn.now_expr();
    let order_by = priority_order_by(conn.kind(), decay_secs);

    // `FOR UPDATE SKIP LOCKED` locks candidate rows for the current
    // transaction on Postgres so concurrent worker scans skip them.
    // SQLite doesn't have this syntax and doesn't need it — the
    // caller's `IMMEDIATE` transaction already serializes writes.
    let lock_clause = if conn.is_postgres() {
        " FOR UPDATE SKIP LOCKED"
    } else {
        ""
    };

    // Pull up to `limit * 2` candidates so the per-row cap check has
    // some slack — if N candidates fail per-slug or per-queue gates
    // we want enough remaining to still hit `limit`.
    let rows = conn.query_all(
        &format!(
            "SELECT id, slug, queue, data, attempt, max_attempts, scheduled_by, created_at, priority, unique_key
             FROM _crap_jobs
             WHERE status = 'pending'
               AND (retry_after IS NULL OR retry_after <= {now})
             {order_by}
             LIMIT {p1}{lock_clause}",
            p1 = conn.placeholder(1)
        ),
        &[DbValue::Integer(
            i64::try_from(limit.saturating_mul(2)).unwrap_or(i64::MAX),
        )],
    )?;

    let running_per_slug = count_running_per_slug(conn)?;
    let running_per_queue = count_running_per_queue(conn)?;

    let mut claimed = Vec::new();
    let mut extra_per_slug: HashMap<String, i64> = HashMap::new();
    let mut extra_per_queue: HashMap<String, i64> = HashMap::new();

    for row in &rows {
        if claimed.len() >= limit {
            break;
        }

        let Some(id) = row.opt_text_at(0) else {
            continue;
        };
        let Some(slug) = row.opt_text_at(1) else {
            continue;
        };
        let queue = row.text_at(2).unwrap_or("default").to_string();

        // Per-slug cap (registry-defined jobs only; system slugs +
        // stale slugs fall through to `u32::MAX` via `per_slug_cap`).
        let slug_cap = i64::from(per_slug_cap(&slug, job_concurrency));
        let slug_current = running_per_slug.get(&slug).copied().unwrap_or(0)
            + extra_per_slug.get(&slug).copied().unwrap_or(0);
        if slug_current >= slug_cap {
            continue;
        }

        // Per-queue cap (absent OR `concurrency = 0` → unlimited).
        if let Some(queue_cap) = queue_concurrency.get(&queue).copied()
            && queue_cap > 0
        {
            let queue_cap = i64::from(queue_cap);
            let queue_current = running_per_queue.get(&queue).copied().unwrap_or(0)
                + extra_per_queue.get(&queue).copied().unwrap_or(0);
            if queue_current >= queue_cap {
                continue;
            }
        }

        // Claim. `AND status = 'pending'` belt-and-suspenders against
        // any drift; on Postgres the FOR UPDATE SKIP LOCKED guarantees
        // we own the row, on SQLite the IMMEDIATE tx prevents writers.
        let p1 = conn.placeholder(1);
        let affected = conn.execute(
            &format!(
                "UPDATE _crap_jobs SET status = 'running', started_at = {now},
                        heartbeat_at = {now}, attempt = attempt + 1
                 WHERE id = {p1} AND status = 'pending'"
            ),
            &[DbValue::Text(id.clone())],
        )?;

        if affected > 0 {
            *extra_per_slug.entry(slug).or_insert(0) += 1;
            *extra_per_queue.entry(queue).or_insert(0) += 1;
            // The row's `attempt` was read pre-UPDATE; the UPDATE
            // bumps it. Reflect post-UPDATE state on the returned
            // `JobRun` so the value matches what `count_running_*`
            // observes.
            let mut job = parse_job_row(row)?;
            job.attempt += 1;
            claimed.push(job);
        }
    }

    Ok(claimed)
}

/// Resolve the per-slug concurrency cap for `slug`. Returns the
/// explicit `JobDefinition::concurrency` when the slug is in the
/// registry, otherwise `u32::MAX` (no per-slug cap). The "no cap"
/// sentinel applies to system jobs (`_system_*` slugs that aren't in
/// `registry.jobs`) and stale jobs whose definition has been removed
/// — both correctly defer to per-queue and global caps instead of
/// being throttled to 1 by an accidental fallback.
fn per_slug_cap(slug: &str, job_concurrency: &HashMap<String, u32>) -> u32 {
    job_concurrency.get(slug).copied().unwrap_or(u32::MAX)
}

/// Build the ORDER BY fragment for pending-job claim queries. With
/// `decay_secs = 0`, returns the static fast path
/// (`priority DESC, created_at ASC`). With `decay_secs > 0`, adds an
/// aging term so older lower-priority jobs eventually get claimed.
///
/// `kind` is the `DbConnection::kind()` discriminant ("sqlite" or
/// "postgres") — the two backends spell "seconds since `created_at`"
/// differently.
fn priority_order_by(kind: &str, decay_secs: u64) -> String {
    if decay_secs == 0 {
        return "ORDER BY priority DESC, created_at ASC".to_string();
    }

    let seconds_since_created = if kind == "postgres" {
        "EXTRACT(EPOCH FROM (now() - created_at))".to_string()
    } else {
        "((julianday('now') - julianday(created_at)) * 86400.0)".to_string()
    };

    #[allow(clippy::cast_precision_loss)]
    let divisor = decay_secs as f64;

    format!("ORDER BY (priority + {seconds_since_created} / {divisor}) DESC, created_at ASC")
}

/// Parse a job row from a SELECT result into a [`JobRun`]. The
/// `attempt` value is taken verbatim from the row — the caller in
/// [`claim_pending_jobs`] adds `+1` to reflect the post-UPDATE state
/// (the UPDATE the caller issues after this parse increments
/// `attempt` in the DB).
fn parse_job_row(row: &DbRow) -> Result<JobRun> {
    let id = row
        .text_at(0)
        .ok_or_else(|| anyhow!("Missing job id"))?
        .to_string();
    let slug = row
        .text_at(1)
        .ok_or_else(|| anyhow!("Missing job slug"))?
        .to_string();
    let queue = row.text_at(2).unwrap_or("default").to_string();
    let data = row.text_at(3).unwrap_or("{}").to_string();
    // Attempt counts above u32::MAX or below 0 indicate a corrupt row;
    // fall back to safe defaults rather than wrapping.
    let attempt = row
        .i64_at(4)
        .and_then(|v| u32::try_from(v).ok())
        .unwrap_or(0);
    let max_attempts = row
        .i64_at(5)
        .and_then(|v| u32::try_from(v).ok())
        .unwrap_or(1);

    // priority at index 8 — clamp out-of-range values to i32 default 0.
    let priority = row
        .i64_at(8)
        .and_then(|v| i32::try_from(v).ok())
        .unwrap_or(0);

    let mut b = JobRun::builder(id, slug)
        .status(JobStatus::Running)
        .queue(queue)
        .data(data)
        .attempt(attempt)
        .max_attempts(max_attempts)
        .priority(priority);

    if let Some(sb) = row.opt_text_at(6) {
        b = b.scheduled_by(sb);
    }
    if let Some(ca) = row.opt_text_at(7) {
        b = b.created_at(ca);
    }
    if let Some(uk) = row.opt_text_at(9) {
        b = b.unique_key(uk);
    }

    Ok(b.build())
}

#[cfg(test)]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::case_sensitive_file_extension_comparisons,
    clippy::items_after_statements,
    clippy::match_wildcard_for_single_variants,
    clippy::missing_panics_doc,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::unreadable_literal,
    clippy::used_underscore_binding
)]
mod tests {
    use super::*;
    use crate::db::query::jobs::insert_job;
    use crate::db::query::jobs::test_helpers::setup_db;

    fn empty_queue_conc() -> HashMap<String, u32> {
        HashMap::new()
    }

    #[test]
    fn test_claim_pending_jobs() {
        let (_dir, conn) = setup_db();
        insert_job(&conn, "job_a", "{}", "cron", 1, "default", 0).unwrap();
        insert_job(&conn, "job_b", "{}", "cron", 1, "default", 0).unwrap();

        let conc: HashMap<String, u32> = HashMap::new();
        let claimed = claim_pending_jobs(&conn, 10, &conc, &empty_queue_conc(), 0).unwrap();
        assert_eq!(claimed.len(), 2);
        assert_eq!(claimed[0].status, JobStatus::Running);

        let claimed2 = claim_pending_jobs(&conn, 10, &conc, &empty_queue_conc(), 0).unwrap();
        assert_eq!(claimed2.len(), 0);
    }

    /// Regression: the SQL claim already does `attempt = attempt + 1` on
    /// the row, and the doc on `fail_job` says the passed `attempt` is
    /// "already incremented by claim". `parse_job_row` previously also
    /// did `.attempt(attempt + 1)`, double-counting.
    #[test]
    fn claim_reports_attempt_count_consistent_with_db_increment() {
        let (_dir, conn) = setup_db();
        insert_job(&conn, "retry_test", "{}", "manual", 3, "default", 0).unwrap();

        let conc: HashMap<String, u32> = HashMap::new();

        let first = claim_pending_jobs(&conn, 10, &conc, &empty_queue_conc(), 0).unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].attempt, 1);

        conn.execute(
            "UPDATE _crap_jobs SET status = 'pending' WHERE id = ?1",
            &[DbValue::Text(first[0].id.clone())],
        )
        .unwrap();

        let second = claim_pending_jobs(&conn, 10, &conc, &empty_queue_conc(), 0).unwrap();
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].attempt, 2);
    }

    #[test]
    fn test_claim_respects_concurrency() {
        let (_dir, conn) = setup_db();
        insert_job(&conn, "limited", "{}", "cron", 1, "default", 0).unwrap();
        insert_job(&conn, "limited", "{}", "cron", 1, "default", 0).unwrap();

        let mut conc = HashMap::new();
        conc.insert("limited".to_string(), 1u32);

        let claimed = claim_pending_jobs(&conn, 10, &conc, &empty_queue_conc(), 0).unwrap();
        assert_eq!(claimed.len(), 1);
    }

    /// Regression: `claim_pending_jobs` should skip jobs whose `retry_after` is in the future.
    #[test]
    fn test_claim_skips_jobs_with_future_retry_after() {
        let (_dir, conn) = setup_db();
        insert_job(&conn, "test", "{}", "manual", 3, "default", 0).unwrap();

        conn.execute(
            "UPDATE _crap_jobs SET retry_after = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '+3600 seconds')",
            &[],
        )
        .unwrap();

        let conc: HashMap<String, u32> = HashMap::new();
        let claimed = claim_pending_jobs(&conn, 10, &conc, &empty_queue_conc(), 0).unwrap();
        assert_eq!(claimed.len(), 0);
    }

    #[test]
    fn test_claim_picks_up_jobs_with_past_retry_after() {
        let (_dir, conn) = setup_db();
        insert_job(&conn, "test", "{}", "manual", 3, "default", 0).unwrap();

        conn.execute(
            "UPDATE _crap_jobs SET retry_after = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-10 seconds')",
            &[],
        )
        .unwrap();

        let conc: HashMap<String, u32> = HashMap::new();
        let claimed = claim_pending_jobs(&conn, 10, &conc, &empty_queue_conc(), 0).unwrap();
        assert_eq!(claimed.len(), 1);
    }

    // ── priority ordering ─────────────────────────────────────────

    #[test]
    fn higher_priority_claimed_first() {
        let (_dir, conn) = setup_db();
        insert_job(&conn, "low", "{}", "manual", 1, "default", 0).unwrap();
        insert_job(&conn, "high", "{}", "manual", 1, "default", 10).unwrap();
        insert_job(&conn, "mid", "{}", "manual", 1, "default", 5).unwrap();

        let conc: HashMap<String, u32> = HashMap::new();
        let claimed = claim_pending_jobs(&conn, 10, &conc, &empty_queue_conc(), 0).unwrap();
        assert_eq!(claimed.len(), 3);

        let slugs: Vec<&str> = claimed.iter().map(|j| j.slug.as_str()).collect();
        assert_eq!(slugs, vec!["high", "mid", "low"]);
    }

    #[test]
    fn fifo_tiebreak_within_priority() {
        let (_dir, conn) = setup_db();
        insert_job(&conn, "first", "{}", "manual", 1, "default", 5).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        insert_job(&conn, "second", "{}", "manual", 1, "default", 5).unwrap();

        let conc: HashMap<String, u32> = HashMap::new();
        let claimed = claim_pending_jobs(&conn, 10, &conc, &empty_queue_conc(), 0).unwrap();
        let slugs: Vec<&str> = claimed.iter().map(|j| j.slug.as_str()).collect();
        assert_eq!(slugs, vec!["first", "second"]);
    }

    #[test]
    fn decay_promotes_old_low_priority_job() {
        let (_dir, conn) = setup_db();
        insert_job(&conn, "old_low", "{}", "manual", 1, "default", 0).unwrap();
        conn.execute(
            "UPDATE _crap_jobs SET created_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-120 seconds') \
             WHERE slug = 'old_low'",
            &[],
        )
        .unwrap();
        insert_job(&conn, "fresh_high", "{}", "manual", 1, "default", 1).unwrap();

        let conc: HashMap<String, u32> = HashMap::new();

        let claimed_no_decay = {
            let (_d2, conn2) = setup_db();
            insert_job(&conn2, "old_low", "{}", "manual", 1, "default", 0).unwrap();
            conn2
                .execute(
                    "UPDATE _crap_jobs SET created_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-120 seconds') \
                     WHERE slug = 'old_low'",
                    &[],
                )
                .unwrap();
            insert_job(&conn2, "fresh_high", "{}", "manual", 1, "default", 1).unwrap();

            claim_pending_jobs(&conn2, 10, &conc, &empty_queue_conc(), 0).unwrap()
        };
        assert_eq!(claimed_no_decay[0].slug, "fresh_high");

        let claimed_decay = claim_pending_jobs(&conn, 10, &conc, &empty_queue_conc(), 60).unwrap();
        assert_eq!(claimed_decay[0].slug, "old_low");
    }

    #[test]
    fn claim_surfaces_priority_in_job_run() {
        let (_dir, conn) = setup_db();
        insert_job(&conn, "test", "{}", "manual", 1, "default", 42).unwrap();

        let conc: HashMap<String, u32> = HashMap::new();
        let claimed = claim_pending_jobs(&conn, 10, &conc, &empty_queue_conc(), 0).unwrap();
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].priority, 42);
    }

    // ── per-queue concurrency caps ────────────────────────────────

    /// Two jobs in `emails` queue with cap=1: only one claimed per tick.
    #[test]
    fn queue_cap_limits_aggregate_claim() {
        let (_dir, conn) = setup_db();
        insert_job(&conn, "send_welcome", "{}", "hook", 1, "emails", 0).unwrap();
        insert_job(&conn, "send_reset", "{}", "hook", 1, "emails", 0).unwrap();

        let conc: HashMap<String, u32> = HashMap::new();
        let mut queue_conc = HashMap::new();
        queue_conc.insert("emails".to_string(), 1u32);

        let claimed = claim_pending_jobs(&conn, 10, &conc, &queue_conc, 0).unwrap();
        assert_eq!(
            claimed.len(),
            1,
            "queue cap=1 must limit aggregate to 1 even with multiple slugs"
        );
    }

    /// Queue without a config entry is unconstrained (default
    /// behavior — regression check that the absent-key path doesn't
    /// accidentally enforce a 0-cap).
    #[test]
    fn queue_without_config_is_unlimited() {
        let (_dir, conn) = setup_db();
        insert_job(&conn, "job_a", "{}", "hook", 1, "reports", 0).unwrap();
        insert_job(&conn, "job_b", "{}", "hook", 1, "reports", 0).unwrap();

        let conc: HashMap<String, u32> = HashMap::new();
        let claimed = claim_pending_jobs(&conn, 10, &conc, &empty_queue_conc(), 0).unwrap();
        assert_eq!(claimed.len(), 2);
    }

    /// Per-slug cap inside a permissive queue still wins. Queue
    /// `emails` cap=10, but `send_welcome` slug cap=1 → only one
    /// `send_welcome` runs even with idle queue budget.
    #[test]
    fn slug_cap_inside_permissive_queue_still_enforced() {
        let (_dir, conn) = setup_db();
        insert_job(&conn, "send_welcome", "{}", "hook", 1, "emails", 0).unwrap();
        insert_job(&conn, "send_welcome", "{}", "hook", 1, "emails", 0).unwrap();

        let mut conc = HashMap::new();
        conc.insert("send_welcome".to_string(), 1u32);
        let mut queue_conc = HashMap::new();
        queue_conc.insert("emails".to_string(), 10u32);

        let claimed = claim_pending_jobs(&conn, 10, &conc, &queue_conc, 0).unwrap();
        assert_eq!(claimed.len(), 1);
    }

    /// Per `QueueConfig` docstring: `concurrency = 0` means
    /// unlimited (treat same as absent from the map). The
    /// scheduler's `loop_runner` filters these out before passing
    /// to claim, but claim itself must be defensive — a test or
    /// downstream caller bypassing the filter shouldn't accidentally
    /// throttle the queue to zero.
    #[test]
    fn queue_cap_zero_means_unlimited() {
        let (_dir, conn) = setup_db();
        insert_job(&conn, "a", "{}", "manual", 1, "default", 0).unwrap();
        insert_job(&conn, "b", "{}", "manual", 1, "default", 0).unwrap();

        let conc: HashMap<String, u32> = HashMap::new();
        let mut queue_conc = HashMap::new();
        queue_conc.insert("default".to_string(), 0u32);

        let claimed = claim_pending_jobs(&conn, 10, &conc, &queue_conc, 0).unwrap();
        assert_eq!(
            claimed.len(),
            2,
            "queue cap = 0 must be interpreted as unlimited; got cap=0 -> 0 claimed bug"
        );
    }

    /// Regression: system jobs (slugs not in `registry.jobs`) must
    /// NOT be capped at the per-slug fallback of 1 — that would
    /// strictest-wins-defeat any per-queue cap set by the operator.
    /// `_system_image_convert` queue=`images` cap=3, three pending →
    /// all three should claim, gated only by the queue cap, not by
    /// an accidental per-slug=1 default.
    #[test]
    fn system_jobs_respect_queue_cap_not_slug_default() {
        let (_dir, conn) = setup_db();
        insert_job(
            &conn,
            "_system_image_convert",
            "{}",
            "system",
            1,
            "images",
            0,
        )
        .unwrap();
        insert_job(
            &conn,
            "_system_image_convert",
            "{}",
            "system",
            1,
            "images",
            0,
        )
        .unwrap();
        insert_job(
            &conn,
            "_system_image_convert",
            "{}",
            "system",
            1,
            "images",
            0,
        )
        .unwrap();

        // Empty per-slug map — simulates the production state where
        // `read_job_concurrency` only carries registry-defined jobs
        // and system slugs are absent.
        let conc: HashMap<String, u32> = HashMap::new();
        let mut queue_conc = HashMap::new();
        queue_conc.insert("images".to_string(), 3u32);

        let claimed = claim_pending_jobs(&conn, 10, &conc, &queue_conc, 0).unwrap();
        assert_eq!(
            claimed.len(),
            3,
            "queue cap=3 must allow 3 concurrent system jobs; per-slug fallback must not cap them at 1"
        );
    }
}
