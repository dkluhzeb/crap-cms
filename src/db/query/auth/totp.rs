//! TOTP enrollment state persistence: the sealed secret, the
//! enrollment-confirmed flag, and the last accepted time step (replay guard).

use anyhow::Result;

use crate::db::{DbConnection, DbValue};

/// A user's TOTP enrollment state.
pub struct TotpState {
    /// Sealed (AES-GCM) base32 secret — `None` until first challenge.
    pub sealed_secret: Option<String>,
    /// Whether enrollment was confirmed by a successful verification.
    pub confirmed: bool,
    /// Last accepted time step; codes at or before it are replays.
    pub last_step: Option<i64>,
}

/// Load a user's TOTP state. `None` when the user row does not exist.
///
/// # Errors
///
/// Returns a backend error if the SELECT fails.
pub fn get_totp_state(
    conn: &dyn DbConnection,
    slug: &str,
    user_id: &str,
) -> Result<Option<TotpState>> {
    let p1 = conn.placeholder(1);

    let Some(row) = conn.query_one(
        &format!(
            "SELECT _totp_secret, _totp_confirmed, _totp_last_step FROM \"{slug}\" WHERE id = {p1}"
        ),
        &[DbValue::Text(user_id.to_string())],
    )?
    else {
        return Ok(None);
    };

    Ok(Some(TotpState {
        sealed_secret: row.opt_text_at(0),
        confirmed: row.i64_at(1).unwrap_or(0) != 0,
        last_step: row.i64_at(2),
    }))
}

/// Store a freshly generated (sealed) secret, guarded against concurrent
/// installs: the UPDATE only applies while the stored secret still equals
/// `expected_old` (`None` = still unenrolled). Resets the confirmed flag
/// and replay guard. Returns whether THIS call won — on `false` the caller
/// must re-read and use the winner's secret, or the provisioning it shows
/// would reference a secret that is no longer stored.
///
/// # Errors
///
/// Returns a backend error if the UPDATE fails.
pub fn set_totp_secret(
    conn: &dyn DbConnection,
    slug: &str,
    user_id: &str,
    sealed_secret: &str,
    expected_old: Option<&str>,
) -> Result<bool> {
    let (p1, p2) = (conn.placeholder(1), conn.placeholder(2));

    let affected = match expected_old {
        None => conn.execute(
            &format!(
                "UPDATE \"{slug}\" SET _totp_secret = {p2}, _totp_confirmed = 0, \
                 _totp_last_step = NULL WHERE id = {p1} AND _totp_secret IS NULL"
            ),
            &[
                DbValue::Text(user_id.to_string()),
                DbValue::Text(sealed_secret.to_string()),
            ],
        )?,
        Some(old) => {
            let p3 = conn.placeholder(3);
            conn.execute(
                &format!(
                    "UPDATE \"{slug}\" SET _totp_secret = {p2}, _totp_confirmed = 0, \
                     _totp_last_step = NULL WHERE id = {p1} AND _totp_secret = {p3}"
                ),
                &[
                    DbValue::Text(user_id.to_string()),
                    DbValue::Text(sealed_secret.to_string()),
                    DbValue::Text(old.to_string()),
                ],
            )?
        }
    };

    Ok(affected > 0)
}

/// Record a successful verification: advance the replay guard to the
/// accepted step and mark enrollment confirmed. **Race-safe**: the UPDATE
/// is conditional on the stored step still being older than `step`, so two
/// concurrent verifications of the same code cannot both record (the loser
/// sees `false` and must fail the login), and an accepted newer step can
/// never be regressed by a concurrent older-step acceptance.
///
/// Returns whether THIS call won the record — treat `false` as
/// verification failure.
///
/// # Errors
///
/// Returns a backend error if the UPDATE fails.
pub fn record_totp_success(
    conn: &dyn DbConnection,
    slug: &str,
    user_id: &str,
    step: i64,
) -> Result<bool> {
    let (p1, p2) = (conn.placeholder(1), conn.placeholder(2));

    let affected = conn.execute(
        &format!(
            "UPDATE \"{slug}\" SET _totp_confirmed = 1, _totp_last_step = {p2} \
             WHERE id = {p1} AND (_totp_last_step IS NULL OR _totp_last_step < {p2})"
        ),
        &[DbValue::Text(user_id.to_string()), DbValue::Integer(step)],
    )?;

    Ok(affected > 0)
}

/// Clear a user's TOTP enrollment entirely (secret, confirmed flag, replay
/// guard) — the next MFA challenge re-provisions from scratch. Used by the
/// `user reset-totp` CLI command.
///
/// # Errors
///
/// Returns a backend error if the UPDATE fails.
pub fn reset_totp(conn: &dyn DbConnection, slug: &str, user_id: &str) -> Result<()> {
    let p1 = conn.placeholder(1);

    conn.execute(
        &format!(
            "UPDATE \"{slug}\" SET _totp_secret = NULL, _totp_confirmed = 0, \
             _totp_last_step = NULL WHERE id = {p1}"
        ),
        &[DbValue::Text(user_id.to_string())],
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, crate::db::BoxedConnection) {
        let dir = tempfile::TempDir::new().unwrap();
        let config = crate::config::CrapConfig::default();
        let pool = crate::db::pool::create_pool(dir.path(), &config).unwrap();
        let conn = pool.get().unwrap();
        conn.execute(
            "CREATE TABLE users (
                id TEXT PRIMARY KEY,
                _totp_secret TEXT,
                _totp_confirmed INTEGER DEFAULT 0,
                _totp_last_step INTEGER
            )",
            &[],
        )
        .unwrap();
        conn.execute("INSERT INTO users (id) VALUES ('u1')", &[])
            .unwrap();
        (dir, conn)
    }

    #[test]
    fn state_round_trip() {
        let (_dir, conn) = setup();

        let state = get_totp_state(&conn, "users", "u1").unwrap().unwrap();
        assert!(state.sealed_secret.is_none());
        assert!(!state.confirmed);
        assert!(state.last_step.is_none());

        assert!(set_totp_secret(&conn, "users", "u1", "sealed-blob", None).unwrap());
        let state = get_totp_state(&conn, "users", "u1").unwrap().unwrap();
        assert_eq!(state.sealed_secret.as_deref(), Some("sealed-blob"));
        assert!(!state.confirmed);

        assert!(record_totp_success(&conn, "users", "u1", 42).unwrap());
        let state = get_totp_state(&conn, "users", "u1").unwrap().unwrap();
        assert!(state.confirmed);
        assert_eq!(state.last_step, Some(42));
    }

    /// Re-provisioning (rotated auth secret restart) resets the confirmed
    /// flag and the replay guard.
    #[test]
    fn new_secret_resets_enrollment() {
        let (_dir, conn) = setup();
        assert!(set_totp_secret(&conn, "users", "u1", "old", None).unwrap());
        assert!(record_totp_success(&conn, "users", "u1", 7).unwrap());

        assert!(set_totp_secret(&conn, "users", "u1", "new", Some("old")).unwrap());

        let state = get_totp_state(&conn, "users", "u1").unwrap().unwrap();
        assert_eq!(state.sealed_secret.as_deref(), Some("new"));
        assert!(!state.confirmed, "fresh secret must be unconfirmed");
        assert!(state.last_step.is_none(), "replay guard must reset");
    }

    /// Regression (review finding): the replay-guard record must be
    /// monotonic and race-safe — a concurrent acceptance of the same or an
    /// older step must lose, never regress `_totp_last_step`.
    #[test]
    fn record_is_monotonic_and_single_winner() {
        let (_dir, conn) = setup();
        set_totp_secret(&conn, "users", "u1", "s", None).unwrap();

        assert!(record_totp_success(&conn, "users", "u1", 10).unwrap());
        // Same step again (the double-accept race): the loser gets false.
        assert!(!record_totp_success(&conn, "users", "u1", 10).unwrap());
        // An older step (window skew race) must not regress the guard.
        assert!(!record_totp_success(&conn, "users", "u1", 9).unwrap());
        let state = get_totp_state(&conn, "users", "u1").unwrap().unwrap();
        assert_eq!(state.last_step, Some(10));

        assert!(record_totp_success(&conn, "users", "u1", 11).unwrap());
    }

    /// Regression (review finding): a concurrent first-enrollment install
    /// must have exactly one winner, so no surface ever shows provisioning
    /// for a secret that is no longer stored.
    #[test]
    fn secret_install_has_single_winner() {
        let (_dir, conn) = setup();

        assert!(set_totp_secret(&conn, "users", "u1", "a", None).unwrap());
        // The racing fresh install loses…
        assert!(!set_totp_secret(&conn, "users", "u1", "b", None).unwrap());
        // …and a stale-compare restart loses too.
        assert!(!set_totp_secret(&conn, "users", "u1", "c", Some("zzz")).unwrap());

        let state = get_totp_state(&conn, "users", "u1").unwrap().unwrap();
        assert_eq!(state.sealed_secret.as_deref(), Some("a"));

        // The rotation restart with the CORRECT old value wins.
        assert!(set_totp_secret(&conn, "users", "u1", "d", Some("a")).unwrap());
    }

    /// `reset_totp` returns the user to the pristine unenrolled state.
    #[test]
    fn reset_clears_everything() {
        let (_dir, conn) = setup();
        set_totp_secret(&conn, "users", "u1", "s", None).unwrap();
        record_totp_success(&conn, "users", "u1", 5).unwrap();

        reset_totp(&conn, "users", "u1").unwrap();

        let state = get_totp_state(&conn, "users", "u1").unwrap().unwrap();
        assert!(state.sealed_secret.is_none());
        assert!(!state.confirmed);
        assert!(state.last_step.is_none());

        // And a fresh guarded install works again.
        assert!(set_totp_secret(&conn, "users", "u1", "s2", None).unwrap());
    }

    #[test]
    fn missing_user_is_none() {
        let (_dir, conn) = setup();
        assert!(get_totp_state(&conn, "users", "ghost").unwrap().is_none());
    }
}
