//! Cron-window dedup: ensures only one worker fires a given cron schedule
//! within a window, even with multiple concurrent workers.

use anyhow::Result;

use crate::db::{DbConnection, DbValue, UpsertSpec};

/// The last recorded fire time for `slug`, or `None` if it has never fired.
///
/// This is the durable half of the cron window: a process-local "last check"
/// starts at process start, so a schedule that came due while the process was
/// down would never be looked at again. Reading the persisted value instead
/// makes the window survive a restart.
///
/// # Errors
///
/// Returns a backend error if the SELECT fails.
pub fn cron_fired_at(conn: &dyn DbConnection, slug: &str) -> Result<Option<String>> {
    let p1 = conn.placeholder(1);

    let row = conn.query_one(
        &format!("SELECT fired_at FROM _crap_cron_fired WHERE slug = {p1}"),
        &[DbValue::Text(slug.to_string())],
    )?;

    Ok(row.and_then(|r| r.opt_text_at(0)))
}

/// Attempt to claim a cron window for a slug. Returns `true` if this
/// instance won the window (and should fire the job), `false` if another
/// instance already fired it.
///
/// One guarded upsert: it inserts the row when the slug has never fired, and
/// otherwise updates it only while the stored `fired_at` is at or before this
/// window's start. The loser of either race changes nothing and reads `false`
/// off the affected-row count.
///
/// Spelled as INSERT-if-absent followed by UPDATE-if-stale, the very first fire
/// of a slug was a race: two workers both find the row absent, and the loser's
/// INSERT fails on the primary key — a failed tick instead of a clean back-off.
/// An IMMEDIATE transaction does not close it, because on Postgres that is a
/// plain `BEGIN` at READ COMMITTED.
///
/// `<=` (not `<`) is load-bearing: the loop advances `last_cron_check` to each
/// tick's `now` and records `fired_at = now`, so the NEXT window's
/// `window_start` equals the PREVIOUS window's stored `fired_at` (the same
/// instant). The window is half-open `(window_start, now]`, so a fire recorded
/// AT `window_start` belongs to the previous window and this window's fire is
/// genuinely new — with strict `<` it was skipped, making a frequent cron (an
/// every-minute one at a 60s interval) fire only every other window. Cross-node
/// dedup still holds: a peer whose window started strictly before another
/// node's recorded fire sees `fired_at > window_start` and correctly backs off.
///
/// # Errors
///
/// Returns a backend error if the statement fails.
pub fn try_claim_cron_window(
    conn: &dyn DbConnection,
    slug: &str,
    fired_at: &str,
    window_start: &str,
) -> Result<bool> {
    let values = format!("{}, {}", conn.placeholder(1), conn.placeholder(2));
    let guard = format!("_crap_cron_fired.fired_at <= {}", conn.placeholder(3));

    let spec = UpsertSpec::builder("_crap_cron_fired", "slug")
        .columns(&["slug", "fired_at"], &values)
        .guard(&guard)
        .build();

    let affected = conn.execute(
        &conn.build_upsert(&spec),
        &[
            DbValue::Text(slug.to_string()),
            DbValue::Text(fired_at.to_string()),
            DbValue::Text(window_start.to_string()),
        ],
    )?;

    Ok(affected > 0)
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use super::*;
    use crate::db::{InMemoryConn, query::test_helpers::CountingConn};

    fn conn() -> InMemoryConn {
        let c = InMemoryConn::open();
        c.setup("CREATE TABLE _crap_cron_fired (slug TEXT PRIMARY KEY, fired_at TEXT);");
        c
    }

    fn claim(c: &InMemoryConn, fired_at: &str, window_start: &str) -> bool {
        try_claim_cron_window(c, "cleanup", fired_at, window_start).unwrap()
    }

    #[test]
    fn a_never_fired_slug_has_no_recorded_window() {
        let c = conn();
        assert_eq!(cron_fired_at(&c, "cleanup").unwrap(), None);
    }

    /// The stored value must come back verbatim: it is compared as a string
    /// against `window_start` in the claim, so any reformatting on the way
    /// out would break the equality case the adjacent-window rule relies on.
    #[test]
    fn a_recorded_fire_reads_back_verbatim() {
        let c = conn();
        claim(
            &c,
            "2026-01-01T00:05:00.123456789+00:00",
            "2026-01-01T00:00:00Z",
        );

        assert_eq!(
            cron_fired_at(&c, "cleanup").unwrap().as_deref(),
            Some("2026-01-01T00:05:00.123456789+00:00")
        );
    }

    #[test]
    fn first_claim_for_a_new_slug_wins() {
        let c = conn();
        assert!(claim(&c, "2026-01-01T00:00:00Z", "2026-01-01T00:00:00Z"));
    }

    /// The loser of the *first* claim backs off instead of failing.
    ///
    /// Two workers racing a slug that has never fired both see no row. With an
    /// insert-then-update pair the loser's INSERT hits the primary key and the
    /// whole tick errors; the guarded upsert turns that collision into a plain
    /// `false` — which is what a second claim inside one window looks like here,
    /// the row having just been created by the winner.
    #[test]
    fn a_second_first_claim_backs_off_instead_of_failing_on_the_primary_key() {
        let c = conn();

        assert!(claim(&c, "2026-01-01T00:05:00Z", "2026-01-01T00:00:00Z"));

        let loser = try_claim_cron_window(
            &c,
            "cleanup",
            "2026-01-01T00:05:01Z",
            "2026-01-01T00:00:00Z",
        );

        assert!(
            matches!(loser, Ok(false)),
            "the loser of a first-fire race must report `false`, not error: {loser:?}"
        );
        assert_eq!(
            cron_fired_at(&c, "cleanup").unwrap().as_deref(),
            Some("2026-01-01T00:05:00Z"),
            "the loser must not overwrite the winner's recorded fire"
        );
    }

    /// The race the sequential tests cannot reproduce is closed by the shape
    /// of the claim: one statement, conflict-aware, guarded. A claim split
    /// back into a read and a write reopens it.
    #[test]
    fn the_claim_is_one_guarded_upsert() {
        let c = conn();
        let spy = CountingConn::new(&c);

        assert!(
            try_claim_cron_window(
                &spy,
                "cleanup",
                "2026-01-01T00:05:00Z",
                "2026-01-01T00:00:00Z"
            )
            .unwrap()
        );

        let executed = spy.executed();
        assert_eq!(
            executed.len(),
            1,
            "the claim must be one statement: {executed:?}"
        );
        assert_eq!(spy.reads(), 0, "the claim must not read first");
        let sql = &executed[0];
        assert!(
            sql.contains("ON CONFLICT") && sql.contains("DO UPDATE") && sql.contains("WHERE"),
            "{sql}"
        );
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

    /// Regression: consecutive loop windows are ADJACENT — the loop records
    /// `fired_at = now` and then advances `last_check` to that same `now`, so
    /// the next window's `window_start` equals the previous window's stored
    /// `fired_at`. With strict `<` the next window was wrongly skipped
    /// (a minutely cron fired every other minute). `<=` claims it.
    #[test]
    fn adjacent_window_boundary_still_claims() {
        let c = conn();
        // Tick N: window (00:00, 00:01], fire recorded at 00:01.
        assert!(claim(&c, "2026-01-01T00:01:00Z", "2026-01-01T00:00:00Z"));
        // Tick N+1: window (00:01, 00:02] — window_start == the stored fire.
        // This is a NEW window and must claim.
        assert!(
            claim(&c, "2026-01-01T00:02:00Z", "2026-01-01T00:01:00Z"),
            "adjacent window (window_start == prior fired_at) must claim"
        );
        // Tick N+2 stays consistent.
        assert!(claim(&c, "2026-01-01T00:03:00Z", "2026-01-01T00:02:00Z"));
    }
}
