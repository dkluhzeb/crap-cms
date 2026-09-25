use axum::{
    extract::State,
    http::HeaderMap,
    response::{IntoResponse, Redirect, Response},
};
use tokio::task::spawn_blocking;
use tracing::warn;

use super::{append_cookies, clear_session_cookies, session_same_site};
use crate::admin::{
    AdminState,
    handlers::shared::paths,
    server::{evaluate_admin_request, session_cookie_token},
};
use crate::core::event::SharedInvalidationTransport;
use crate::db::DbPool;
use crate::service::{self, ServiceContext, auth::Resolution};

/// Bump the user's `_session_version` so the already-issued JWT dies
/// server-side (see the handler doc for why). Failures are logged, never
/// surfaced — the user still gets logged out client-side either way.
fn bump_session_version_blocking(
    pool: &DbPool,
    transport: SharedInvalidationTransport,
    collection: &str,
    sub: &str,
) {
    let conn = match pool.write() {
        Ok(conn) => conn,
        Err(e) => {
            warn!(
                user = %sub,
                collection = %collection,
                error = ?e,
                "logout could not get a connection to bump _session_version; the issued JWT remains valid until exp"
            );
            return;
        }
    };

    let ctx = ServiceContext::slug_only(collection)
        .conn(&conn)
        .invalidation_transport(Some(transport))
        .build();

    if let Err(e) = service::auth::bump_session_version(&ctx, sub) {
        // Don't surface to the user — they still get logged
        // out client-side. But operators need visibility:
        // a failed bump means future requests with the same
        // JWT will still work, defeating the logout's intent.
        warn!(
            user = %sub,
            collection = %collection,
            error = ?e,
            "logout could not bump _session_version; the issued JWT remains valid until exp"
        );
    }
}

/// Blocking half of logout: resolve the session cookie through the same
/// evaluator the auth middleware uses, and retire the session it names.
///
/// The logout route is mounted outside the auth layer on purpose — an
/// expired or otherwise dead session must still be able to clear its
/// cookies — so no middleware has resolved a principal for this request.
/// Resolving it here is what makes the bump actually run; a request whose
/// cookie no longer authenticates has nothing to retire and simply falls
/// through to the cookie clearing.
fn revoke_session_blocking(state: &AdminState, headers: &HeaderMap) {
    let session = session_cookie_token(headers);

    let resolution = match evaluate_admin_request(state, headers, None, session) {
        Ok(resolution) => resolution,
        Err(e) => {
            warn!(
                error = ?e,
                "logout could not evaluate the session; the issued JWT remains valid until exp"
            );
            return;
        }
    };

    let Resolution::Authenticated(auth) = resolution else {
        return;
    };

    let claims = &auth.user.claims;

    bump_session_version_blocking(
        &state.infra.pool,
        state.infra.invalidation_transport.clone(),
        &claims.collection,
        &claims.sub,
    );
}

/// POST /admin/logout — clear cookies, redirect to login.
///
/// Also bumps the user's `_session_version` server-side so the
/// already-issued JWT becomes `Invalid(StaleSession)` on any
/// future use. Cookie clearing alone leaves the JWT exploitable
/// by anyone who has captured it (XSS, MITM with cookie scraped,
/// device-handoff scenarios). The bump closes that window.
///
/// The invalidation transport is attached so `bump_session_version`
/// can also tear down the user's open live-update streams — the bump
/// alone only blocks *new* requests, and an already-connected stream
/// never re-reads `_session_version` (mirrors lock / password-reset).
pub async fn logout_action(State(state): State<AdminState>, headers: HeaderMap) -> Response {
    let revoke_state = state.clone();
    let _ = spawn_blocking(move || {
        revoke_session_blocking(&revoke_state, &headers);
    })
    .await;

    let same_site = session_same_site(&state);
    let cookies = clear_session_cookies(state.config.admin.dev_mode, same_site);
    let mut response = Redirect::to(&paths::login_with_success("success_logout")).into_response();
    append_cookies(&mut response, &cookies);
    response
}
