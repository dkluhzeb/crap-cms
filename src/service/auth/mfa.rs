//! MFA (email second-factor) code persistence + verification.

use crate::{
    db::query,
    service::{ServiceContext, ServiceError},
};

/// Store an MFA code for a user.
///
/// # Errors
///
/// Returns a backend error if the DB connection or persistence
/// fails.
pub fn set_mfa_code(
    ctx: &ServiceContext,
    id: &str,
    code: &str,
    expiry: i64,
) -> Result<(), ServiceError> {
    let conn = ctx.resolve_conn()?;
    query::set_mfa_code(conn.as_ref(), ctx.slug, id, code, expiry)?;
    Ok(())
}

/// Verify an MFA code. Returns true if valid and not expired.
///
/// # Errors
///
/// Returns a backend error if the DB connection or query fails.
pub fn verify_mfa_code(ctx: &ServiceContext, id: &str, code: &str) -> Result<bool, ServiceError> {
    let conn = ctx.resolve_conn()?;
    Ok(query::verify_mfa_code(conn.as_ref(), ctx.slug, id, code)?)
}
