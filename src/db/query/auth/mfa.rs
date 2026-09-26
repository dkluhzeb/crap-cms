//! Multi-factor authentication code management.

use anyhow::Result;
use chrono::Utc;

use crate::{
    core::{
        Builder,
        auth::{hash_mfa_code, mfa_code_matches},
    },
    db::{DbConnection, DbValue},
};

/// One user's MFA code — the code to store, or the attempt to judge — with
/// the `[auth] secret` that keys its stored digest.
#[derive(Builder)]
pub struct MfaCode<'a> {
    /// The user the code belongs to.
    #[builder(required)]
    pub user_id: &'a str,
    /// The 6-digit code; only its keyed digest is stored.
    #[builder(required)]
    pub code: &'a str,
    /// `[auth] secret`, which keys the digest.
    #[builder(required)]
    pub auth_secret: &'a str,
}

/// Store `mfa`'s code for its user in the auth collection `slug`, expiring at
/// `exp` (Unix timestamp). Overwrites any existing code.
///
/// The auth secret keys the stored digest: six digits is a small enough
/// preimage space that a bare hash of one is invertible by table lookup, so
/// without the key the stored form would still be the credential.
///
/// # Errors
///
/// Returns a backend error if the UPDATE fails.
pub fn set_mfa_code(conn: &dyn DbConnection, slug: &str, mfa: &MfaCode, exp: i64) -> Result<()> {
    let MfaCode {
        user_id,
        code,
        auth_secret,
    } = *mfa;
    let (p1, p2, p3) = (
        conn.placeholder(1),
        conn.placeholder(2),
        conn.placeholder(3),
    );
    conn.execute(
        &format!("UPDATE \"{slug}\" SET _mfa_code = {p2}, _mfa_code_exp = {p3} WHERE id = {p1}"),
        &[
            DbValue::Text(user_id.to_string()),
            // The keyed digest is stored, never the code itself.
            DbValue::Text(hash_mfa_code(auth_secret, code)),
            DbValue::Integer(exp),
        ],
    )?;
    Ok(())
}

/// The stored code an attempt read: its keyed digest and expiry.
struct StoredCode {
    digest: String,
    exp: Option<i64>,
}

/// Read the user's stored code, if one is outstanding.
fn read_code(conn: &dyn DbConnection, slug: &str, user_id: &str) -> Result<Option<StoredCode>> {
    let p1 = conn.placeholder(1);

    let Some(row) = conn.query_one(
        &format!("SELECT _mfa_code, _mfa_code_exp FROM \"{slug}\" WHERE id = {p1}"),
        &[DbValue::Text(user_id.to_string())],
    )?
    else {
        return Ok(None);
    };

    Ok(row.opt_text_at(0).map(|digest| StoredCode {
        digest,
        exp: row.i64_at(1),
    }))
}

/// Consume the code an attempt read: clear it only if it is still the stored
/// one. Returns whether THIS attempt consumed it — of concurrent attempts
/// that read the same code, exactly one does; the others find it gone.
fn consume_code(conn: &dyn DbConnection, slug: &str, user_id: &str, digest: &str) -> Result<bool> {
    let (p1, p2) = (conn.placeholder(1), conn.placeholder(2));

    let affected = conn.execute(
        &format!(
            "UPDATE \"{slug}\" SET _mfa_code = NULL, _mfa_code_exp = NULL \
             WHERE id = {p1} AND _mfa_code = {p2}"
        ),
        &[
            DbValue::Text(user_id.to_string()),
            DbValue::Text(digest.to_string()),
        ],
    )?;

    Ok(affected > 0)
}

/// Judge the attempt `mfa` against its user's stored code in the auth
/// collection `slug`. Returns true if the code matches and has not expired.
///
/// **Single-use semantics**: every attempt consumes the stored code, success
/// or failure. Without this, an attacker holding a valid MFA-pending JWT
/// could brute-force the 6-digit code at request rate (1M codes / 5-min
/// window). User-visible cost: a typo means re-requesting a fresh code.
///
/// **One verdict per code, even under concurrency**: the consumption is a
/// conditional UPDATE on the code this attempt read, so of N concurrent
/// attempts that all read the live code, exactly one consumes it and gets a
/// verdict — the rest are refused as if the code were already gone. A burst
/// of guesses therefore gets one guess per issued code, not N (and a
/// concurrent double-submit of the right code completes the login once).
///
/// **Constant-time compare**: the byte comparison goes through
/// `subtle::ConstantTimeEq` so a remote attacker cannot recover the
/// stored code byte-by-byte from response-time variance.
///
/// # Errors
///
/// Returns a backend error if the SELECT or the consuming UPDATE fails.
pub fn verify_mfa_code(conn: &dyn DbConnection, slug: &str, mfa: &MfaCode) -> Result<bool> {
    let MfaCode {
        user_id,
        code,
        auth_secret,
    } = *mfa;
    let now = Utc::now().timestamp();

    let Some(stored) = read_code(conn, slug, user_id)? else {
        return Ok(false);
    };

    if !consume_code(conn, slug, user_id, &stored.digest)? {
        return Ok(false);
    }

    // The column holds a digest keyed with the auth secret, so the presented
    // code is hashed the same way to compare.
    let codes_match = mfa_code_matches(auth_secret, code, &stored.digest);

    // Expire at the boundary (`now == exp` is already expired), matching the
    // reset/verification token checks (`now >= exp`). Fail-closed by one second
    // rather than honoring a code at its exact expiry timestamp.
    let not_expired = stored.exp.is_some_and(|exp| now < exp);

    Ok(not_expired && codes_match)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CrapConfig;

    /// Any fixed key — the tests only need store and verify to agree on it.
    const TEST_SECRET: &str = "test-auth-secret";

    fn setup() -> (tempfile::TempDir, crate::db::BoxedConnection) {
        let dir = tempfile::TempDir::new().unwrap();
        let config = CrapConfig::default();
        let pool = crate::db::pool::create_pool(dir.path(), &config).unwrap();
        let conn = pool.get().unwrap();
        conn.execute(
            "CREATE TABLE users (
                id TEXT PRIMARY KEY,
                _revision INTEGER NOT NULL DEFAULT 0,
                _mfa_code TEXT,
                _mfa_code_exp INTEGER
            )",
            &[],
        )
        .unwrap();
        conn.execute("INSERT INTO users (id) VALUES ('u1')", &[])
            .unwrap();
        (dir, conn)
    }

    /// `user_id`'s code `code`, keyed with the test secret.
    fn attempt<'a>(user_id: &'a str, code: &'a str) -> MfaCode<'a> {
        MfaCode::builder(user_id, code, TEST_SECRET).build()
    }

    /// Future-proof exp: 1 day from now (in seconds).
    fn future_exp() -> i64 {
        Utc::now().timestamp() + 86_400
    }

    #[test]
    fn correct_code_verifies_and_clears() {
        let (_dir, conn) = setup();
        set_mfa_code(&conn, "users", &attempt("u1", "123456"), future_exp()).unwrap();

        assert!(verify_mfa_code(&conn, "users", &attempt("u1", "123456")).unwrap());

        // Code cleared — second verify returns false even with the same code.
        assert!(!verify_mfa_code(&conn, "users", &attempt("u1", "123456")).unwrap());
    }

    /// Regression: the original implementation cleared the stored code
    /// only on success, leaving a valid code in the DB after a wrong
    /// guess. An attacker holding the MFA-pending JWT could then
    /// brute-force the 6-digit code at request rate. The fix makes the
    /// code single-use — wrong guess clears it too, forcing the user
    /// (and the attacker) to request a fresh code.
    #[test]
    fn wrong_code_clears_on_failed_attempt() {
        let (_dir, conn) = setup();
        set_mfa_code(&conn, "users", &attempt("u1", "123456"), future_exp()).unwrap();

        assert!(!verify_mfa_code(&conn, "users", &attempt("u1", "999999")).unwrap());

        // Even with the CORRECT code, the second attempt must fail —
        // the stored code was cleared by the wrong guess.
        assert!(
            !verify_mfa_code(&conn, "users", &attempt("u1", "123456")).unwrap(),
            "code must be single-use; correct guess after a wrong one must fail",
        );
    }

    #[test]
    fn expired_code_returns_false_and_clears() {
        let (_dir, conn) = setup();
        // Expiration in the distant past.
        set_mfa_code(&conn, "users", &attempt("u1", "123456"), 1).unwrap();

        assert!(!verify_mfa_code(&conn, "users", &attempt("u1", "123456")).unwrap());

        // Re-setting after expiry works fine — clear left columns NULL,
        // not in some half-state.
        set_mfa_code(&conn, "users", &attempt("u1", "654321"), future_exp()).unwrap();
        assert!(verify_mfa_code(&conn, "users", &attempt("u1", "654321")).unwrap());
    }

    /// Regression: reading the code, comparing, and clearing it were separate
    /// statements, so concurrent attempts all read the live code before any
    /// cleared it — a burst of N guesses got N comparisons against one issued
    /// code. Of two attempts that read the same code, only the one that
    /// consumes it gets a verdict.
    #[test]
    fn concurrent_attempts_on_one_code_get_one_verdict() {
        let (_dir, conn) = setup();
        set_mfa_code(&conn, "users", &attempt("u1", "123456"), future_exp()).unwrap();

        let first = read_code(&conn, "users", "u1").unwrap().expect("stored");
        let second = read_code(&conn, "users", "u1").unwrap().expect("stored");

        assert!(consume_code(&conn, "users", "u1", &first.digest).unwrap());
        assert!(
            !consume_code(&conn, "users", "u1", &second.digest).unwrap(),
            "the second attempt on the same code must find it consumed"
        );
    }

    /// A code re-issued between an attempt's read and its consumption is not
    /// consumed by that stale attempt.
    #[test]
    fn a_stale_attempt_does_not_consume_a_reissued_code() {
        let (_dir, conn) = setup();
        set_mfa_code(&conn, "users", &attempt("u1", "123456"), future_exp()).unwrap();
        let stale = read_code(&conn, "users", "u1").unwrap().expect("stored");

        set_mfa_code(&conn, "users", &attempt("u1", "654321"), future_exp()).unwrap();

        assert!(!consume_code(&conn, "users", "u1", &stale.digest).unwrap());
        assert!(verify_mfa_code(&conn, "users", &attempt("u1", "654321")).unwrap());
    }

    #[test]
    fn no_code_set_returns_false() {
        let (_dir, conn) = setup();
        assert!(!verify_mfa_code(&conn, "users", &attempt("u1", "anything")).unwrap());
    }

    #[test]
    fn missing_user_returns_false() {
        let (_dir, conn) = setup();
        assert!(!verify_mfa_code(&conn, "users", &attempt("u_nonexistent", "123456")).unwrap());
    }
}
