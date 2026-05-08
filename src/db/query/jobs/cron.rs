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
