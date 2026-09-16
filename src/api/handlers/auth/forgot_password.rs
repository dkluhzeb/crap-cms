//! Forgot password handler — generate reset token and queue email.

use tonic::{Request, Response};

use crate::{
    api::{content, handlers::ContentService},
    core::{collection::Auth, normalize_email},
    service::ResetTarget,
};

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Initiate a password reset flow -- generates a token and sends a reset email.
    /// Always returns success to prevent leaking user existence.
    pub(in crate::api::handlers) fn forgot_password_impl(
        &self,
        request: Request<content::ForgotPasswordRequest>,
    ) -> Response<content::ForgotPasswordResponse> {
        let ip = request
            .remote_addr()
            .map_or_else(|| "unknown".to_string(), |a| a.ip().to_string());
        let req = request.into_inner();

        let ok_response = Response::new(content::ForgotPasswordResponse {});

        // Key the per-email limiter on the address in its stored form so
        // spelling variants of one account share a bucket — `find_by_email`
        // compares that form, so a raw-email key would let an attacker sidestep
        // the per-account reset-flood limit by rotating the spelling.
        let email_key = normalize_email(&req.email);

        // Atomically record this attempt against both limiters and bail if
        // either is now over threshold — one operation per limiter, closing the
        // concurrent-bypass race the is_blocked + separate record split left
        // open. Both are evaluated (not short-circuited) so each counter
        // advances. The generic success response leaks nothing on a block.
        let email_blocked = self.forgot_password_limiter.check_and_block(&email_key);
        let ip_blocked = self.ip_forgot_password_limiter.check_and_block(&ip);
        if email_blocked || ip_blocked {
            return ok_response;
        }

        let Ok(def) = self.get_collection_def(&req.collection) else {
            return ok_response;
        };

        if !def.is_auth_collection()
            || !def.auth.as_ref().is_some_and(Auth::forgot_password_enabled)
            || !def.auth.as_ref().is_some_and(Auth::password_login_enabled)
        {
            return ok_response;
        }

        // The token and the email job are minted together in one transaction
        // inside the spawned task, so a crash can never leave a live reset
        // token whose link was never queued for delivery.
        self.infra.email.send_reset(
            ResetTarget::builder(
                self.infra.pool.clone(),
                self.infra.locale_config.clone(),
                req.collection,
                def,
                req.email,
                self.reset_token_expiry,
            )
            .build(),
        );

        ok_response
    }
}
