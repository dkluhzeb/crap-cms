//! The admin MFA challenge step — issued when a verified authentication must
//! complete the collection's second factor before a session is minted. Shared
//! by the password login and the external auth callbacks, so both hand the
//! user the same pending-MFA flow (TOTP, email code, or custom delivery).
//!
//! A codec over the service chokepoint
//! ([`service::auth::issue_mfa_challenge`], shared with the gRPC `Login`):
//! this surface only encodes the challenge as the pending cookie and the
//! redirect to the MFA page.

use axum::response::{IntoResponse, Redirect, Response};

use crate::{
    admin::{
        AdminState,
        handlers::{
            auth::{append_cookies, mfa_pending_cookie},
            shared::paths,
        },
    },
    core::collection::Surface,
    service::auth::{self, ChallengeRefusal, ChallengeRequest, LoginVerified},
};

/// The translation key of the error the login page shows for a refusal.
pub(in crate::admin::handlers) fn refusal_error_key(refusal: ChallengeRefusal) -> &'static str {
    match refusal {
        ChallengeRefusal::Throttled => "error_mfa_too_many_codes",
        ChallengeRefusal::Internal => "error_internal",
    }
}

/// Issue the MFA challenge for a verified user of `collection` and redirect to
/// the MFA page with the pending cookie set. `email` is the address the
/// pending token carries and a code is delivered to. The pending token is
/// bound to the admin surface, so only the admin MFA page can complete it.
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
    let request = ChallengeRequest::builder(
        collection,
        verified,
        email,
        Surface::Admin,
        AsRef::<str>::as_ref(&state.config.auth.secret),
        &state.forgot_password_limiter,
    )
    .build();

    let challenge = auth::issue_mfa_challenge(&state.infra, &request)?;

    let cookie = mfa_pending_cookie(&challenge.pending_token, state.config.admin.dev_mode);
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
            refusal_error_key(ChallengeRefusal::Throttled),
            "error_mfa_too_many_codes"
        );
        assert_eq!(
            refusal_error_key(ChallengeRefusal::Internal),
            "error_internal"
        );
    }
}
