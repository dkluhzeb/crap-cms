//! Resend-verification handler — issue a fresh verification link and email it.

use std::sync::Arc;

use tonic::{Request, Response};

use crate::core::collection::Auth;
use crate::{
    api::{content, handlers::ContentService},
    core::normalize_email,
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
        let ip = request
            .remote_addr()
            .map_or_else(|| "unknown".to_string(), |a| a.ip().to_string());
        let req = request.into_inner();

        let ok_response = Response::new(content::ResendVerificationResponse {});

        // Own keyspace, not the forgot-password limiters: a burst of resends
        // must not drain the budget a legitimate password reset from the same
        // address or IP needs. Mirrors the admin route and the two other
        // token endpoints.
        //
        // Normalized per-email key: the account lookup is case-insensitive, so
        // a raw-email key would let casing variants each get a fresh budget.
        let email_key = normalize_email(&req.email);

        let email_blocked = self
            .forgot_password_limiter
            .rescoped("resend_verification")
            .check_and_block(&email_key);
        let ip_blocked = self
            .ip_forgot_password_limiter
            .rescoped("ip_resend_verification")
            .check_and_block(&ip);
        if email_blocked || ip_blocked {
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
