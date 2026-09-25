//! Minting a session token — the one place every surface signs one.
//!
//! The admin login, the admin MFA completion, the auth callbacks, the admin
//! session refresh, and the gRPC `Login` / `VerifyMfa` RPCs all mint through
//! [`mint_session`], so every session token carries the same stamps: the
//! surface that minted it and whether it satisfied the collection's second
//! factor. The request-time evaluator reads the second-factor stamp to refuse
//! a session on a surface whose MFA gate requires what it never proved.

use anyhow::anyhow;
use chrono::Utc;

use crate::{
    core::{Builder, Slug, auth::ClaimsBuilder, collection::Surface},
    service::{AppInfra, ServiceError},
};

/// Who a session is minted for, and how it was established.
#[derive(Builder)]
pub struct SessionGrant<'a> {
    /// The user's document id.
    #[builder(required)]
    user_id: &'a str,
    /// The auth collection the session binds to.
    #[builder(required)]
    collection: &'a str,
    /// The user's email, carried in the claims.
    #[builder(required)]
    email: &'a str,
    /// The user's current session version.
    #[builder(required)]
    session_version: u64,
    /// The surface minting the session.
    #[builder(required)]
    surface: Surface,
    /// Whether the session satisfied the second factor: the user completed
    /// the MFA step, or signed in through an auth callback the collection
    /// exempts. Unset, it did not.
    mfa: bool,
    /// Unix timestamp of the **original** authentication. Unset, it is now —
    /// a fresh login; a refresh forwards the previous token's value so the
    /// absolute session ceiling is measured from the original login.
    auth_time: Option<u64>,
    /// The absolute session ceiling (seconds from `auth_time`) the surface
    /// enforces; `0` = none. The token never outlives it.
    absolute_max_age: u64,
}

/// A signed session token.
#[derive(Debug)]
pub struct MintedSession {
    /// The signed JWT.
    pub token: String,
    /// Its expiry (Unix timestamp).
    pub exp: u64,
    /// Seconds from now until `exp` — the lifetime a cookie carrying it gets.
    pub lifetime: u64,
}

/// Expiry for a session token issued at `now`: `now + expiry`, capped at
/// `auth_time + max_age` so a refreshed token never outlives the session's
/// absolute ceiling. `max_age = 0` disables the cap.
fn session_exp(now: u64, expiry: u64, auth_time: u64, max_age: u64) -> u64 {
    let exp = now.saturating_add(expiry);

    if max_age == 0 {
        return exp;
    }

    exp.min(auth_time.saturating_add(max_age))
}

/// The session token lifetime for `collection`: its own `token_expiry`, or
/// the global `[auth] token_expiry` when it sets none — the one place the two
/// are resolved. A session is only ever minted for a registered auth
/// collection — a token for any other would be refused on first use anyway.
fn token_expiry(infra: &AppInfra, collection: &str) -> Result<u64, ServiceError> {
    infra
        .registry
        .get_collection(collection)
        .and_then(|def| def.auth.as_ref())
        .map(|auth| auth.token_lifetime(infra.token_expiry))
        .ok_or_else(|| {
            ServiceError::Internal(anyhow!(
                "cannot mint a session for '{collection}': not an auth collection"
            ))
        })
}

/// Mint and sign the session token `grant` describes, stamped with its
/// minting surface and second-factor state.
///
/// # Errors
///
/// Returns an internal error when the collection is not a registered auth
/// collection, or when claims building or signing fails.
pub fn mint_session(
    infra: &AppInfra,
    grant: &SessionGrant<'_>,
) -> Result<MintedSession, ServiceError> {
    let now = Utc::now().timestamp().max(0).cast_unsigned();
    let auth_time = grant.auth_time.unwrap_or(now);

    let expiry = token_expiry(infra, grant.collection)?;
    let exp = session_exp(now, expiry, auth_time, grant.absolute_max_age);

    let claims = ClaimsBuilder::new(grant.user_id, Slug::new(grant.collection))
        .email(grant.email)
        .exp(exp)
        .auth_time(auth_time)
        .session_version(grant.session_version)
        .surface(grant.surface)
        .mfa(grant.mfa)
        .build()
        .map_err(ServiceError::Internal)?;

    let token = infra
        .token_provider
        .create_token(&claims)
        .map_err(ServiceError::Internal)?;

    Ok(MintedSession {
        token,
        exp,
        lifetime: exp.saturating_sub(now),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A refresh near the end of the absolute session lifetime must not mint
    /// a token that outlives it.
    #[test]
    fn session_exp_is_capped_by_the_absolute_max_age() {
        // Logged in at 0, refreshing at 3000 with a 2h token and a 1h ceiling.
        assert_eq!(session_exp(3000, 7200, 0, 3600), 3600);
        // Far from the ceiling the normal expiry applies.
        assert_eq!(session_exp(1000, 100, 0, 3600), 1100);
        // No ceiling configured.
        assert_eq!(session_exp(3000, 7200, 0, 0), 10_200);
    }
}
