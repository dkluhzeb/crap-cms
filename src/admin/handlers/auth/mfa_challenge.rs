//! The admin MFA challenge step — issued when a verified authentication must
//! complete the collection's second factor before a session is minted. Shared
//! by the password login and the external auth callbacks, so both hand the
//! user the same pending-MFA flow (TOTP, email code, or custom delivery).

use std::sync::Arc;

use axum::response::{IntoResponse, Redirect, Response};
use tokio::task;
use tracing::{error, warn};

use crate::{
    admin::{
        AdminState,
        handlers::{
            auth::{append_cookies, is_totp_collection, mfa_pending_cookie},
            shared::paths,
        },
    },
    core::rate_limit::MFA_ISSUE_KEYSPACE,
    service::auth::{self, LoginVerified},
};

/// Why no MFA challenge was issued.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::admin::handlers) enum ChallengeRefusal {
    /// The user's code-issuance budget is spent.
    Throttled,
    /// The pending token could not be minted.
    Internal,
}

impl ChallengeRefusal {
    /// The translation key of the error the login page shows.
    pub(in crate::admin::handlers) fn error_key(self) -> &'static str {
        match self {
            Self::Throttled => "error_mfa_too_many_codes",
            Self::Internal => "error_internal",
        }
    }
}

/// Throttle MFA code ISSUANCE per user (email / custom delivery): `true` when
/// this user's budget is spent. The login limiter is cleared on each
/// successful authentication, so without this a credential-holder could loop
/// the login to flood the victim's inbox. A code is single-use and expires
/// with the pending token, so there is no earlier code to fall back on: over
/// budget, the challenge is refused outright rather than handed out with no
/// code that can complete it.
fn issuance_throttled(state: &AdminState, verified: &LoginVerified) -> bool {
    let throttled = state
        .forgot_password_limiter
        .rescoped(MFA_ISSUE_KEYSPACE)
        .check_and_block(verified.user.id.as_ref());

    if throttled {
        warn!(user = %verified.user.id, "MFA code issuance throttled");
    }

    throttled
}

/// Generate a 6-digit code, store it, and deliver it (built-in email or the
/// collection's `mfa_deliver` hook) in the background — the shared body the
/// gRPC challenge flow also uses.
fn spawn_code_delivery(
    state: &AdminState,
    collection: &str,
    verified: &LoginVerified,
    email: &str,
) {
    let code = auth::generate_mfa_code();
    let infra = Arc::clone(&state.infra);
    let slug = collection.to_string();
    let user = verified.user.clone();
    let email = email.to_string();
    let auth_secret = AsRef::<str>::as_ref(&state.config.auth.secret).to_string();

    task::spawn_blocking(move || {
        auth::deliver_mfa_code(&infra, &auth_secret, &slug, &user, &email, &code);
    });
}

/// Issue the MFA challenge for a verified user of `collection`: mint the
/// short-lived pending token, start code delivery (nothing to deliver for
/// TOTP — the user verifies against their authenticator app, and enrollment
/// is resolved when the MFA page renders), and redirect to the MFA page with
/// the pending cookie set. `email` is the address the pending token carries
/// and a code is delivered to.
///
/// # Errors
///
/// [`ChallengeRefusal`] when no challenge could be issued — the caller
/// answers with its own login error.
pub(in crate::admin::handlers) fn issue_mfa_challenge(
    state: &AdminState,
    collection: &str,
    verified: &LoginVerified,
    email: &str,
) -> Result<Response, ChallengeRefusal> {
    let is_totp = is_totp_collection(state, collection);

    if !is_totp && issuance_throttled(state, verified) {
        return Err(ChallengeRefusal::Throttled);
    }

    let token = auth::mint_mfa_pending_token(
        &state.infra,
        collection,
        &verified.user,
        email,
        verified.session_version,
    )
    .inspect_err(|e| error!("MFA pending token error: {e}"))
    .map_err(|_| ChallengeRefusal::Internal)?;

    if !is_totp {
        spawn_code_delivery(state, collection, verified, email);
    }

    let cookie = mfa_pending_cookie(&token, state.config.admin.dev_mode);
    let mut response = Redirect::to(&paths::mfa_with_collection(collection)).into_response();

    append_cookies(&mut response, &[cookie]);

    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each refusal maps to the login page's translated error.
    #[test]
    fn refusals_map_to_their_login_errors() {
        assert_eq!(
            ChallengeRefusal::Throttled.error_key(),
            "error_mfa_too_many_codes"
        );
        assert_eq!(ChallengeRefusal::Internal.error_key(), "error_internal");
    }
}
