//! Multi-factor authentication code management.

use anyhow::Result;

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

/// Verify a MFA code for a user. Returns true if the code matches and has not expired.
/// Clears the code on success to prevent reuse.
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

    if exp < now || stored_code != code {
        return Ok(false);
    }

    clear_mfa_code(conn, slug, user_id)?;
    Ok(true)
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
