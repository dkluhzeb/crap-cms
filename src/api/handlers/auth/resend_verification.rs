//! Resend-verification handler — issue a fresh verification link and email it.

use std::sync::Arc;

use tonic::{Request, Response};

use crate::{
    api::{content, handlers::ContentService, request_client_ip},
    core::{
        collection::Auth,
        login_email_key,
        rate_limit::{
            AttemptBudget, IP_RESEND_VERIFICATION_KEYSPACE, RESEND_VERIFICATION_KEYSPACE,
        },
    },
    service::ResendTarget,
};

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Issue a fresh email-verification link and queue the email.
    ///
    /// Always returns success. A caller cannot tell an unverified account
    /// from a verified one, a locked one, or an address that was never
    /// registered — the response is the same in every case.
    pub(in crate::api::handlers) fn resend_verification_impl(
        &self,
        request: Request<content::ResendVerificationRequest>,
    ) -> Response<content::ResendVerificationResponse> {
        let client = request_client_ip(&request, &self.server_config);
        let req = request.into_inner();

        let ok_response = Response::new(content::ResendVerificationResponse {});

        // Same success whatever happens, so an address longer than any
        // deliverable one is refused — before it is keyed, throttled or looked
        // up — without leaking anything. The normalized key makes casing
        // variants of one address share a budget.
        let Some(email_key) = login_email_key(&req.email) else {
            return ok_response;
        };

        // Own keyspace, not the forgot-password limiters: a burst of resends
        // must not drain the budget a legitimate password reset from the same
        // address or IP needs. Mirrors the admin route and the two other
        // token endpoints. IP budget first (see `AttemptBudget`).
        let email_limiter = self
            .forgot_password_limiter
            .rescoped(RESEND_VERIFICATION_KEYSPACE);
        let ip_limiter = self
            .ip_forgot_password_limiter
            .rescoped(IP_RESEND_VERIFICATION_KEYSPACE);

        if AttemptBudget::new(&ip_limiter, &email_limiter).check_and_block(&client, &email_key) {
            return ok_response;
        }

        let Ok(def) = self.get_collection_def(&req.collection) else {
            return ok_response;
        };

        if !def.is_auth_collection() || !def.auth.as_ref().is_some_and(Auth::requires_verify_email)
        {
            return ok_response;
        }

        self.infra.email.resend_verification(
            ResendTarget::builder(
                self.infra.pool.clone(),
                self.infra.locale_config.clone(),
                req.collection,
                Arc::clone(&def),
                req.email,
            )
            .build(),
        );

        ok_response
    }
}
