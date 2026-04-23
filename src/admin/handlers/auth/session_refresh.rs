use anyhow::{self, bail};
use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    response::{IntoResponse, Response},
};
use chrono::Utc;
use tokio::task;
use tracing::error;

use crate::{
    admin::{
        AdminState,
        handlers::auth::{
            append_cookies, create_session_token, session_cookies, session_same_site,
        },
    },
    core::auth::Claims,
    db::{DbPool, query::is_valid_identifier},
    service::{self, ServiceContext},
};

/// Outcome of the absolute-max-age check during session refresh.
#[derive(Debug, PartialEq)]
enum RefreshDecision {
    /// Issue a new token with this `auth_time` preserved.
    Extend(u64),
    /// Force the user to re-authenticate — cap exceeded or no timestamp to
    /// anchor the cap to.
    Refuse,
}

/// Decide whether a token can be refreshed, given the configured absolute
/// session max age and the current time. Extracted as a pure function so
/// the policy can be unit-tested independently of the HTTP handler.
///
/// Resolution:
/// - Tokens minted after `auth_time` landed carry the original login time
///   directly → use it.
/// - Legacy tokens fall back to `iat` (refreshed on every reissue, so less
///   accurate, but acceptable as a transitional fallback).
/// - Tokens with neither claim cannot safely be extended — refuse.
///
/// `max_age = 0` disables the cap.
fn resolve_original_auth_time(claims: &Claims, max_age: u64, now: u64) -> RefreshDecision {
    let Some(original) = claims.auth_time.or(claims.iat) else {
        return RefreshDecision::Refuse;
    };

    if max_age > 0 && now.saturating_sub(original) > max_age {
        return RefreshDecision::Refuse;
    }

    RefreshDecision::Extend(original)
}

/// Verify that the user still exists, is not locked, and return the current session version.
///
/// Returns `Ok((locked, session_version))` on success.
fn check_session_status(pool: &DbPool, slug: &str, user_id: &str) -> anyhow::Result<(bool, u64)> {
    if !is_valid_identifier(slug) {
        bail!("Invalid collection slug");
    }

    let conn = pool.get()?;
    let ctx = ServiceContext::slug_only(slug).conn(&conn).build();

    // Verify user still exists — is_locked and get_session_version both
    // return defaults (false/0) for missing rows, so a deleted user would
    // silently pass all checks and refresh their session indefinitely.
    if !service::auth::user_exists(&ctx, user_id).map_err(|e| e.into_anyhow())? {
        bail!("User no longer exists");
    }

    let locked = service::auth::is_locked(&ctx, user_id).map_err(|e| e.into_anyhow())?;
    let session_version =
        service::auth::get_session_version(&ctx, user_id).map_err(|e| e.into_anyhow())?;

    Ok((locked, session_version))
}

/// POST /admin/api/session-refresh — issue a fresh JWT if the current one is still valid.
pub async fn session_refresh(State(state): State<AdminState>, request: Request<Body>) -> Response {
    let claims = match request.extensions().get::<Claims>() {
        Some(c) => c.clone(),
        None => return StatusCode::UNAUTHORIZED.into_response(),
    };

    let pool = state.pool.clone();
    let slug = claims.collection.clone();
    let user_id = claims.sub.clone();

    let check_result =
        task::spawn_blocking(move || check_session_status(&pool, &slug, &user_id)).await;

    let session_version = match check_result {
        Ok(Ok((true, _))) => return StatusCode::UNAUTHORIZED.into_response(),
        Ok(Ok((false, sv))) => sv,
        Ok(Err(e)) => {
            error!("Session refresh check: {}", e);

            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        Err(e) => {
            error!("Session refresh task error: {}", e);

            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    // Reject tokens with stale session version (password was changed)
    if claims.session_version != session_version {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let now = Utc::now().timestamp().max(0) as u64;
    let max_age = state.config.auth.session_absolute_max_age;

    let original_auth_time = match resolve_original_auth_time(&claims, max_age, now) {
        RefreshDecision::Extend(t) => t,
        RefreshDecision::Refuse => return StatusCode::UNAUTHORIZED.into_response(),
    };

    let session = match create_session_token(
        &state,
        claims.sub.to_string(),
        &claims.collection,
        claims.email,
        session_version,
        original_auth_time,
    ) {
        Ok(s) => s,
        Err(e) => {
            error!("Session refresh: {}", e);
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let cookies = session_cookies(
        &session.token,
        session.expiry,
        session.exp,
        state.config.admin.dev_mode,
        session_same_site(&state),
    );
    let mut response = StatusCode::NO_CONTENT.into_response();

    append_cookies(&mut response, &cookies);

    response
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::core::auth::ClaimsBuilder;

    fn base_claims() -> Claims {
        ClaimsBuilder::new("u", "users")
            .email("a@b.com")
            .exp(9_999_999_999)
            .build()
            .unwrap()
    }

    #[test]
    fn resolve_extends_using_auth_time_when_present() {
        let mut claims = base_claims();
        claims.auth_time = Some(1_000_000);
        claims.iat = Some(1_500_000); // should be ignored in favour of auth_time

        let decision = resolve_original_auth_time(&claims, 0, 2_000_000);
        assert_eq!(decision, RefreshDecision::Extend(1_000_000));
    }

    #[test]
    fn resolve_falls_back_to_iat_for_legacy_tokens() {
        let mut claims = base_claims();
        claims.auth_time = None;
        claims.iat = Some(1_500_000);

        let decision = resolve_original_auth_time(&claims, 0, 2_000_000);
        assert_eq!(decision, RefreshDecision::Extend(1_500_000));
    }

    #[test]
    fn resolve_refuses_when_no_timestamp_is_present() {
        // A hand-crafted claim with neither auth_time nor iat is unsafe to
        // extend — the handler must refuse rather than grant an unbounded
        // new session.
        let mut claims = base_claims();
        claims.auth_time = None;
        claims.iat = None;

        assert_eq!(
            resolve_original_auth_time(&claims, 0, 2_000_000),
            RefreshDecision::Refuse,
        );
    }

    #[test]
    fn resolve_refuses_when_max_age_exceeded() {
        let mut claims = base_claims();
        claims.auth_time = Some(1_000_000);

        // 1 day elapsed, cap is 1 hour.
        let decision = resolve_original_auth_time(&claims, 3600, 1_086_400);
        assert_eq!(decision, RefreshDecision::Refuse);
    }

    #[test]
    fn resolve_extends_when_within_max_age() {
        let mut claims = base_claims();
        claims.auth_time = Some(1_000_000);

        // 1 hour elapsed, cap is 1 day.
        let decision = resolve_original_auth_time(&claims, 86400, 1_003_600);
        assert_eq!(decision, RefreshDecision::Extend(1_000_000));
    }

    #[test]
    fn resolve_max_age_zero_disables_cap() {
        let mut claims = base_claims();
        claims.auth_time = Some(1_000_000);

        // 1 year elapsed, cap is disabled (max_age = 0).
        let decision = resolve_original_auth_time(&claims, 0, 1_000_000 + 31_536_000);
        assert_eq!(decision, RefreshDecision::Extend(1_000_000));
    }

    #[test]
    fn resolve_saturating_sub_guards_clock_skew() {
        // If `now` is somehow before the original auth_time (clock went
        // backwards, or a token was minted from a server with a fast
        // clock), `saturating_sub` returns 0 — treated as within the cap.
        let mut claims = base_claims();
        claims.auth_time = Some(2_000_000);

        let decision = resolve_original_auth_time(&claims, 3600, 1_000_000);
        assert_eq!(decision, RefreshDecision::Extend(2_000_000));
    }
}
