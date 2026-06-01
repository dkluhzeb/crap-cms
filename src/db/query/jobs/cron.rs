//! Cron-window dedup: ensures only one worker fires a given cron schedule
//! within a window, even with multiple concurrent workers.

use anyhow::Result;

use crate::db::{DbConnection, DbValue};

/// Attempt to claim a cron window for a slug. Returns `true` if this
/// instance won the window (and should fire the job), `false` if another
/// instance already fired it.
///
/// Uses an atomic upsert: inserts or updates `_crap_cron_fired` only if
/// the stored `fired_at` is before `window_start`. If the row was
/// already updated (by another instance in this window), the WHERE clause
/// prevents the update and `affected == 0`.
///
/// Must be called inside an IMMEDIATE/serializable transaction.
///
/// # Errors
///
/// Returns a backend error if the INSERT or UPDATE fails.
pub fn try_claim_cron_window(
    conn: &dyn DbConnection,
    slug: &str,
    fired_at: &str,
    window_start: &str,
) -> Result<bool> {
    let p1 = conn.placeholder(1);
    let p2 = conn.placeholder(2);
    let p3 = conn.placeholder(3);

    // Try INSERT first (new slug, never fired before)
    let inserted = conn.execute(
        &format!(
            "INSERT INTO _crap_cron_fired (slug, fired_at)
             SELECT {p1}, {p2}
             WHERE NOT EXISTS (SELECT 1 FROM _crap_cron_fired WHERE slug = {p1})"
        ),
        &[
            DbValue::Text(slug.to_string()),
            DbValue::Text(fired_at.to_string()),
        ],
    )?;

    if inserted > 0 {
        return Ok(true);
    }

    // Row exists — try to update only if last fire was before window start
    let updated = conn.execute(
        &format!(
            "UPDATE _crap_cron_fired SET fired_at = {p2}
             WHERE slug = {p1} AND fired_at < {p3}"
        ),
        &[
            DbValue::Text(slug.to_string()),
            DbValue::Text(fired_at.to_string()),
            DbValue::Text(window_start.to_string()),
        ],
    )?;

    Ok(updated > 0)
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use super::*;
    use crate::db::InMemoryConn;

    fn conn() -> InMemoryConn {
        let c = InMemoryConn::open();
        c.setup("CREATE TABLE _crap_cron_fired (slug TEXT PRIMARY KEY, fired_at TEXT);");
        c
    }

    fn claim(c: &InMemoryConn, fired_at: &str, window_start: &str) -> bool {
        try_claim_cron_window(c, "cleanup", fired_at, window_start).unwrap()
    }

    #[test]
    fn first_claim_for_a_new_slug_wins() {
        let c = conn();
        assert!(claim(&c, "2026-01-01T00:00:00Z", "2026-01-01T00:00:00Z"));
    }

    #[test]
    fn second_claim_in_the_same_window_loses() {
        let c = conn();
        assert!(claim(&c, "2026-01-01T00:05:00Z", "2026-01-01T00:00:00Z"));
        // Same window: the stored fire (00:05) is not before window_start
        // (00:00), so the update is blocked and the second worker loses.
        assert!(!claim(&c, "2026-01-01T00:06:00Z", "2026-01-01T00:00:00Z"));
    }

    #[test]
    fn a_claim_in_the_next_window_wins_again() {
        let c = conn();
        assert!(claim(&c, "2026-01-01T00:05:00Z", "2026-01-01T00:00:00Z"));
        // Next window: window_start advances past the stored fire (00:05 <
        // 00:10), so the update succeeds.
        assert!(claim(&c, "2026-01-01T00:15:00Z", "2026-01-01T00:10:00Z"));
    }
}
