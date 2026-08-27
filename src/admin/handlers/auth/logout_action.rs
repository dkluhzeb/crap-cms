use axum::{
    Extension,
    extract::State,
    response::{IntoResponse, Redirect, Response},
};
use tokio::task::spawn_blocking;
use tracing::warn;

use super::{append_cookies, clear_session_cookies, session_same_site};
use crate::admin::{AdminState, handlers::shared::paths};
use crate::core::AuthUser;
use crate::service::{self, ServiceContext};

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
pub async fn logout_action(
    State(state): State<AdminState>,
    auth_user: Option<Extension<AuthUser>>,
) -> Response {
    if let Some(Extension(user)) = auth_user {
        let pool = state.infra.pool.clone();
        let transport = state.infra.invalidation_transport.clone();
        let collection = user.claims.collection.clone();
        let sub = user.claims.sub.clone();
        let _ = spawn_blocking(move || {
            let Ok(conn) = pool.get() else { return };
            let ctx = ServiceContext::slug_only(&collection)
                .conn(&conn)
                .invalidation_transport(Some(transport))
                .build();
            if let Err(e) = service::auth::bump_session_version(&ctx, &sub) {
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
        })
        .await;
    }

    let same_site = session_same_site(&state);
    let cookies = clear_session_cookies(state.config.admin.dev_mode, same_site);
    let mut response = Redirect::to(paths::LOGIN).into_response();
    append_cookies(&mut response, &cookies);
    response
}
