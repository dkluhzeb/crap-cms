//! Multi-factor authentication code management.

use anyhow::Result;
use subtle::ConstantTimeEq;

use crate::db::{DbConnection, DbValue};

/// Store a MFA code for a user. Overwrites any existing code.
pub fn set_mfa_code(
    conn: &dyn DbConnection,
    slug: &str,
    user_id: &str,
    code: &str,
    exp: i64,
) -> Result<()> {
    let (p1, p2, p3) = (
        conn.placeholder(1),
        conn.placeholder(2),
        conn.placeholder(3),
    );
    conn.execute(
        &format!("UPDATE \"{slug}\" SET _mfa_code = {p2}, _mfa_code_exp = {p3} WHERE id = {p1}"),
        &[
            DbValue::Text(user_id.to_string()),
            DbValue::Text(code.to_string()),
            DbValue::Integer(exp),
        ],
    )?;
    Ok(())
}

/// Verify a MFA code for a user. Returns true if the code matches and
/// has not expired.
///
/// **Single-use semantics**: the stored code is cleared on every verify
/// attempt, success or failure. Without this, an attacker holding a
/// valid MFA-pending JWT could brute-force the 6-digit code at request
/// rate (1M codes / 5-min window). The original code expected a
/// rate-limiter at the handler level; making the code single-use is a
/// stricter, simpler guarantee that doesn't depend on additional state.
/// User-visible cost: a typo means re-requesting a fresh code.
///
/// **Constant-time compare**: the byte comparison goes through
/// `subtle::ConstantTimeEq` so a remote attacker cannot recover the
/// stored code byte-by-byte from response-time variance.
pub fn verify_mfa_code(
    conn: &dyn DbConnection,
    slug: &str,
    user_id: &str,
    code: &str,
) -> Result<bool> {
    let now = chrono::Utc::now().timestamp();
    let p1 = conn.placeholder(1);

    let Some(row) = conn.query_one(
        &format!("SELECT _mfa_code, _mfa_code_exp FROM \"{slug}\" WHERE id = {p1}"),
        &[DbValue::Text(user_id.to_string())],
    )?
    else {
        return Ok(false);
    };

    let stored_code = match row.get_value(0) {
        Some(DbValue::Text(s)) => s.clone(),
        _ => return Ok(false),
    };
    let exp = match row.get_value(1) {
        Some(DbValue::Integer(n)) => *n,
        _ => return Ok(false),
    };

    // Compare BEFORE clearing so we know whether to return success.
    let codes_match = stored_code.as_bytes().ct_eq(code.as_bytes());
    let not_expired = exp >= now;

    // Always clear — single-use regardless of outcome. Prevents
    // brute-force across attempts.
    clear_mfa_code(conn, slug, user_id)?;

    Ok(not_expired && bool::from(codes_match))
}

/// Clear the MFA code for a user.
fn clear_mfa_code(conn: &dyn DbConnection, slug: &str, user_id: &str) -> Result<()> {
    let p1 = conn.placeholder(1);
    conn.execute(
        &format!("UPDATE \"{slug}\" SET _mfa_code = NULL, _mfa_code_exp = NULL WHERE id = {p1}"),
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

    /// Future-proof exp: 1 day from now (in seconds).
    fn future_exp() -> i64 {
        chrono::Utc::now().timestamp() + 86_400
    }

    #[test]
    fn correct_code_verifies_and_clears() {
        let (_dir, conn) = setup();
        set_mfa_code(&conn, "users", "u1", "123456", future_exp()).unwrap();

        assert!(verify_mfa_code(&conn, "users", "u1", "123456").unwrap());

        // Code cleared — second verify returns false even with the same code.
        assert!(!verify_mfa_code(&conn, "users", "u1", "123456").unwrap());
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
        set_mfa_code(&conn, "users", "u1", "123456", future_exp()).unwrap();

        assert!(!verify_mfa_code(&conn, "users", "u1", "999999").unwrap());

        // Even with the CORRECT code, the second attempt must fail —
        // the stored code was cleared by the wrong guess.
        assert!(
            !verify_mfa_code(&conn, "users", "u1", "123456").unwrap(),
            "code must be single-use; correct guess after a wrong one must fail",
        );
    }

    #[test]
    fn expired_code_returns_false_and_clears() {
        let (_dir, conn) = setup();
        // Expiration in the distant past.
        set_mfa_code(&conn, "users", "u1", "123456", 1).unwrap();

        assert!(!verify_mfa_code(&conn, "users", "u1", "123456").unwrap());

        // Re-setting after expiry works fine — clear left columns NULL,
        // not in some half-state.
        set_mfa_code(&conn, "users", "u1", "654321", future_exp()).unwrap();
        assert!(verify_mfa_code(&conn, "users", "u1", "654321").unwrap());
    }

    #[test]
    fn no_code_set_returns_false() {
        let (_dir, conn) = setup();
        assert!(!verify_mfa_code(&conn, "users", "u1", "anything").unwrap());
    }

    #[test]
    fn missing_user_returns_false() {
        let (_dir, conn) = setup();
        assert!(!verify_mfa_code(&conn, "users", "u_nonexistent", "123456").unwrap());
    }
}
