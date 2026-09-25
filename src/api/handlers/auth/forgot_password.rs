//! Forgot password handler — generate reset token and queue email.

use tonic::{Request, Response};

use crate::{
    api::{content, handlers::ContentService, request_client_ip},
    core::{collection::Auth, login_email_key, rate_limit::AttemptBudget},
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
        let client = request_client_ip(&request, &self.server_config);
        let req = request.into_inner();

        let ok_response = Response::new(content::ForgotPasswordResponse {});

        // The response is the generic success whatever happens, so refusing an
        // address longer than any deliverable one — before it is keyed,
        // throttled or looked up — leaks nothing. The per-email key is the
        // address in its stored form (the form `find_by_email` compares), so
        // spelling variants of one account share one reset-flood budget.
        let Some(email_key) = login_email_key(&req.email) else {
            return ok_response;
        };

        // Atomically record this attempt, IP budget first (see
        // `AttemptBudget`); a block returns the same generic success.
        let budget = AttemptBudget::new(
            &self.ip_forgot_password_limiter,
            &self.forgot_password_limiter,
        );
        if budget.check_and_block(&client, &email_key) {
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
