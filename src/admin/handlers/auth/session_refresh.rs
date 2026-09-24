use anyhow::{self, bail};
use axum::{
    body::Body,
    extract::State,
    http::{Extensions, Request, StatusCode},
    response::{IntoResponse, Response},
};
use chrono::Utc;
use tokio::task;
use tracing::error;

use crate::{
    admin::{
        AdminState,
        handlers::auth::{
            SessionGrant, append_cookies, create_session_token, session_cookies, session_same_site,
        },
    },
    core::auth::{Claims, TokenUse},
    db::{DbPool, query::is_valid_identifier},
    service::{self, ServiceContext, ServiceError, auth::ResolvedMethod},
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

    // At exactly `max_age` the capped expiry would be `now`: a token dead on
    // arrival. Refuse instead.
    if max_age > 0 && now.saturating_sub(original) >= max_age {
        return RefreshDecision::Refuse;
    }

    RefreshDecision::Extend(original)
}

/// The claims a refresh may extend: those of a request the **session
/// cookie** authenticated. A bearer token or a custom strategy authenticates
/// a request without the cookie session this endpoint extends — refreshing
/// one would exchange that credential for a fresh session cookie (for a
/// strategy, a signed JWT surviving the strategy credential's revocation).
/// The claims must also be a session's own, never a strategy's in-memory
/// ones.
fn refreshable_claims(extensions: &Extensions) -> Option<Claims> {
    let Some(ResolvedMethod::SessionCookie { .. }) = extensions.get::<ResolvedMethod>() else {
        return None;
    };

    let claims = extensions.get::<Claims>()?;

    (claims.token_use == TokenUse::Session).then(|| claims.clone())
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
    if !service::auth::user_exists(&ctx, user_id).map_err(ServiceError::into_anyhow)? {
        bail!("User no longer exists");
    }

    let locked = service::auth::is_locked(&ctx, user_id).map_err(ServiceError::into_anyhow)?;
    let session_version =
        service::auth::get_session_version(&ctx, user_id).map_err(ServiceError::into_anyhow)?;

    Ok((locked, session_version))
}

/// Map the status check to the session version a refresh may continue, or
/// the status refusing it: a locked account is `401`, a failed check `500`.
fn session_version_outcome(check: anyhow::Result<(bool, u64)>) -> Result<u64, StatusCode> {
    match check {
        Ok((false, session_version)) => Ok(session_version),
        Ok((true, _)) => Err(StatusCode::UNAUTHORIZED),
        Err(e) => {
            error!("Session refresh check: {}", e);

            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// Run [`check_session_status`] for the claims' user off the async runtime.
async fn current_session_version(state: &AdminState, claims: &Claims) -> Result<u64, StatusCode> {
    let pool = state.infra.pool.clone();
    let slug = claims.collection.clone();
    let user_id = claims.sub.clone();

    let check = task::spawn_blocking(move || check_session_status(&pool, &slug, &user_id))
        .await
        .inspect_err(|e| error!("Session refresh task error: {}", e))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    session_version_outcome(check)
}

/// Mint the refreshed session — same user, same original `auth_time` — and
/// answer `204` carrying its cookies.
fn refreshed_session_response(
    state: &AdminState,
    claims: Claims,
    session_version: u64,
    original_auth_time: u64,
) -> Response {
    let grant = SessionGrant::builder(
        claims.sub.to_string(),
        &claims.collection,
        claims.email,
        session_version,
    )
    .auth_time(Some(original_auth_time))
    .build();

    let Ok(session) =
        create_session_token(state, grant).inspect_err(|e| error!("Session refresh: {}", e))
    else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };

    let cookies = session_cookies(
        &session.token,
        session.expiry,
        session.exp,
        state.config.admin.dev_mode,
        session_same_site(state),
    );
    let mut response = StatusCode::NO_CONTENT.into_response();

    append_cookies(&mut response, &cookies);

    response
}

/// POST /admin/api/session-refresh — issue a fresh JWT if the current one is still valid.
pub async fn session_refresh(State(state): State<AdminState>, request: Request<Body>) -> Response {
    let Some(claims) = refreshable_claims(request.extensions()) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };

    let session_version = match current_session_version(&state, &claims).await {
        Ok(v) => v,
        Err(status) => return status.into_response(),
    };

    // Reject tokens with stale session version (password was changed)
    if claims.session_version != session_version {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let now = Utc::now().timestamp().max(0).cast_unsigned();
    let max_age = state.config.auth.session_absolute_max_age;

    let RefreshDecision::Extend(original_auth_time) =
        resolve_original_auth_time(&claims, max_age, now)
    else {
        return StatusCode::UNAUTHORIZED.into_response();
    };

    refreshed_session_response(&state, claims, session_version, original_auth_time)
}

#[cfg(test)]
mod tests {
    use anyhow::anyhow;

    use super::*;

    use crate::core::{Slug, auth::ClaimsBuilder};

    /// A live, unlocked account continues at its current session version.
    #[test]
    fn an_unlocked_account_continues_at_its_session_version() {
        assert_eq!(session_version_outcome(Ok((false, 7))), Ok(7));
    }

    /// A locked account is refused, not refreshed.
    #[test]
    fn a_locked_account_is_unauthorized() {
        assert_eq!(
            session_version_outcome(Ok((true, 7))),
            Err(StatusCode::UNAUTHORIZED)
        );
    }

    /// A failed check (missing user, DB error) fails closed as a server error.
    #[test]
    fn a_failed_check_is_a_server_error() {
        assert_eq!(
            session_version_outcome(Err(anyhow!("User no longer exists"))),
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        );
    }

    fn base_claims() -> Claims {
        ClaimsBuilder::new("u", "users")
            .email("a@b.com")
            .exp(9_999_999_999)
            .build()
            .unwrap()
    }

    fn extensions_with(claims: Claims, via: ResolvedMethod) -> Extensions {
        let mut extensions = Extensions::new();

        extensions.insert(claims);
        extensions.insert(via);

        extensions
    }

    /// A cookie-authenticated session is refreshable.
    #[test]
    fn a_cookie_session_is_refreshable() {
        let via = ResolvedMethod::SessionCookie {
            collection: Slug::new("users"),
        };

        let claims = refreshable_claims(&extensions_with(base_claims(), via));

        assert_eq!(claims.map(|c| c.sub.to_string()), Some("u".to_string()));
    }

    /// Regression: a request a custom strategy authenticated carried
    /// internal claims the refresh endpoint minted a signed session JWT
    /// from, exchanging the strategy credential for a long-lived token.
    #[test]
    fn a_strategy_request_is_not_refreshable() {
        let mut claims = base_claims();
        claims.token_use = TokenUse::Strategy;
        let via = ResolvedMethod::Strategy {
            collection: Slug::new("users"),
            name: "sso".to_string(),
        };

        assert!(refreshable_claims(&extensions_with(claims, via)).is_none());
    }

    /// Strategy claims are refused even if paired with a cookie marker.
    #[test]
    fn strategy_claims_are_never_refreshable() {
        let mut claims = base_claims();
        claims.token_use = TokenUse::Strategy;
        let via = ResolvedMethod::SessionCookie {
            collection: Slug::new("users"),
        };

        assert!(refreshable_claims(&extensions_with(claims, via)).is_none());
    }

    /// A bearer-authenticated request has no cookie session to extend.
    #[test]
    fn a_bearer_request_is_not_refreshable() {
        let via = ResolvedMethod::Bearer {
            collection: Slug::new("users"),
        };

        assert!(refreshable_claims(&extensions_with(base_claims(), via)).is_none());
    }

    /// Claims without a resolved method (no middleware ran) are refused.
    #[test]
    fn claims_without_a_resolved_method_are_not_refreshable() {
        let mut extensions = Extensions::new();
        extensions.insert(base_claims());

        assert!(refreshable_claims(&extensions).is_none());
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

    /// At exactly the ceiling there is no lifetime left to issue.
    #[test]
    fn resolve_refuses_at_exactly_max_age() {
        let mut claims = base_claims();
        claims.auth_time = Some(1_000_000);

        let decision = resolve_original_auth_time(&claims, 3600, 1_003_600);
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
