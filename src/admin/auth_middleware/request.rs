//! The credentials an admin-surface request presents, and their resolution
//! through the shared auth evaluator for handlers outside the auth middleware.

use anyhow::{Context as _, Result};
use axum::http::{
    HeaderMap,
    header::{AUTHORIZATION, COOKIE},
};

use crate::{
    admin::{AdminState, handlers::auth::SESSION_COOKIE, server::extract_cookie},
    core::collection::Surface,
    service::auth::{AuthRequest, EvaluateDeps, Resolution, evaluate},
};

use super::middleware::headers_to_map;

/// The token of an `Authorization: Bearer …` header, if the request has one.
pub(crate) fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .filter(|token| !token.is_empty())
}

/// The session JWT of the session cookie, if the request has one.
pub(crate) fn session_cookie_token(headers: &HeaderMap) -> Option<&str> {
    let cookie_header = headers
        .get(COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    extract_cookie(cookie_header, SESSION_COOKIE)
}

/// Resolve an admin-surface request's credentials through the shared auth
/// evaluator, against a pooled connection of its own. A request with no
/// credential, where no strategy could accept it, is anonymous without one.
///
/// # Errors
///
/// Returns an error when a request that needs a database connection can't have
/// one.
pub(crate) fn evaluate_admin_request(
    state: &AdminState,
    headers: &HeaderMap,
    bearer: Option<&str>,
    session_cookie: Option<&str>,
) -> Result<Resolution> {
    // Nothing to evaluate: a busy pool must not refuse a public request.
    if bearer.is_none() && session_cookie.is_none() && !state.infra.registry.has_any_strategy() {
        return Ok(Resolution::Anonymous);
    }

    let conn = state
        .infra
        .pool
        .get()
        .context("connection for auth evaluation")?;
    let header_map = headers_to_map(headers);

    Ok(evaluate(
        &AuthRequest {
            surface: Surface::Admin,
            bearer_token: bearer,
            session_cookie_token: session_cookie,
            headers: &header_map,
        },
        &EvaluateDeps {
            registry: &state.infra.registry,
            token_provider: state.infra.token_provider.as_ref(),
            hook_runner: &state.infra.hook_runner,
            conn: &conn,
            locale_config: &state.infra.locale_config,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admin::test_state::test_admin_state;

    /// Regression: a request without credentials checked out a database
    /// connection before finding nothing to evaluate, so a busy pool refused a
    /// public page or file.
    #[test]
    fn a_request_without_credentials_needs_no_connection() {
        let state = test_admin_state();
        let _held: Vec<_> = (0..4).map(|_| state.infra.pool.get().unwrap()).collect();

        let resolution = evaluate_admin_request(&state, &HeaderMap::new(), None, None).unwrap();

        assert!(matches!(resolution, Resolution::Anonymous));
    }
}
