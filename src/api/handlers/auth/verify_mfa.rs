//! `VerifyMfa` handler — complete an MFA-gated login.
//!
//! Counterpart of the admin `/admin/mfa` action over the same chokepoints:
//! the pending token minted by `Login` (purpose-bound `MfaPending` claims, so
//! a session token can't be replayed here and this token can't be used as a
//! session), the stored single-use 6-digit code, and the shared `mfa` /
//! `ip_mfa` guess limiters.

use std::sync::Arc;

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{
        content,
        handlers::{ContentService, proto::document_to_proto},
    },
    core::collection::Surface,
    db::query::MfaCode,
    service::{
        AppInfra, ServiceError,
        auth::{self, SessionGrant, mint_session},
    },
};

/// Owned inputs for the second-factor verification `spawn_blocking` body.
struct VerifyCodeInput {
    infra: Arc<AppInfra>,
    auth_secret: String,
    slug: String,
    user_id: String,
    code: String,
}

/// Verify the second factor: TOTP against the sealed shared secret, or the
/// stored (single-use, expiring) code — dispatched on the collection's MFA
/// mode by the shared service chokepoint.
fn verify_code_blocking(input: &VerifyCodeInput) -> anyhow::Result<bool> {
    let attempt = MfaCode::builder(&input.user_id, &input.code, &input.auth_secret).build();

    auth::verify_second_factor(&input.infra, &input.slug, &attempt)
        .map_err(ServiceError::into_anyhow)
}

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Complete an MFA-gated login: validate the challenge token and the
    /// emailed code, then mint the JWT the plain `Login` would have issued.
    pub(in crate::api::handlers) async fn verify_mfa_impl(
        &self,
        request: Request<content::VerifyMfaRequest>,
    ) -> Result<Response<content::LoginResponse>, Status> {
        let ip = request
            .remote_addr()
            .map_or_else(|| "unknown".to_string(), |a| a.ip().to_string());
        let req = request.into_inner();

        // Validate the pending token FIRST (cheap, no DB) — purpose-bound to
        // MfaPending, issued by this surface's `Login`, expiring with the
        // 5-minute window.
        let Ok(pending) = self
            .infra
            .token_provider
            .validate_pending_token(&req.mfa_challenge, Surface::Grpc)
        else {
            return Err(Status::unauthenticated("Invalid or expired MFA challenge"));
        };

        if pending.collection.as_ref() != req.collection {
            return Err(Status::unauthenticated("Invalid or expired MFA challenge"));
        }

        // Throttle code guessing: the 6-digit code lives in a 10^6 space
        // behind a reusable pending token. The `mfa`/`ip_mfa` limiters are
        // SHARED with the admin MFA page (same keyspace), so the guessing
        // budget is per identity/IP across surfaces, and independent of the
        // login limiter. Both are evaluated so each records the attempt.
        let user_id = pending.sub.to_string();
        let user_blocked = self.mfa_limiter.check_and_block(&user_id);
        let ip_blocked = self.ip_mfa_limiter.check_and_block(&ip);
        if user_blocked || ip_blocked {
            return Err(Status::resource_exhausted(
                "Too many MFA attempts. Please try again later.",
            ));
        }

        let input = VerifyCodeInput {
            infra: Arc::clone(&self.infra),
            auth_secret: self.auth_secret.clone(),
            slug: req.collection.clone(),
            user_id: user_id.clone(),
            code: req.code.clone(),
        };

        // Classified, not flattened: a busy pool is UNAVAILABLE (retryable)
        // like everywhere else, rather than an INTERNAL a client won't retry.
        let verified = task::spawn_blocking(move || verify_code_blocking(&input))
            .await
            .inspect_err(|e| error!("VerifyMfa task error: {e}"))
            .map_err(|_| Status::internal("Internal error"))?
            .inspect_err(|e| error!("VerifyMfa error: {e:#}"))
            .map_err(|e| Status::from(ServiceError::classify(e, &self.db_kind)))?;

        if !verified {
            return Err(Status::unauthenticated("Invalid MFA code"));
        }

        // Re-resolve the user fail-closed (lock/delete/session bump inside
        // the pending window invalidates the challenge).
        let infra = Arc::clone(&self.infra);
        let claims_for_load = pending.clone();

        let Some(resolved) =
            task::spawn_blocking(move || auth::reload_authenticated_user(&infra, &claims_for_load))
                .await
                .inspect_err(|e| error!("VerifyMfa load task error: {e}"))
                .map_err(|_| Status::internal("Internal error"))?
        else {
            return Err(Status::unauthenticated("Invalid or expired MFA challenge"));
        };

        // The second factor is proven: the session carries the stamp, so it
        // authenticates every surface the collection's gate would require it on.
        let grant = SessionGrant::builder(
            &pending.sub,
            &req.collection,
            &pending.email,
            pending.session_version,
            Surface::Grpc,
        )
        .mfa(true)
        .build();

        let token = mint_session(&self.infra, &grant)
            .inspect_err(|e| error!("Session mint error: {e}"))
            .map_err(|_| Status::internal("Internal error"))?
            .token;

        // This identity just completed its second factor — clear its guess
        // budget and refund the shared per-IP attempt (mirrors the login
        // limiter semantics: a success must not wipe other identities'
        // failures from the same IP).
        self.mfa_limiter.clear(&user_id);
        self.ip_mfa_limiter.refund(&ip);

        Ok(Response::new(content::LoginResponse {
            token,
            user: Some(document_to_proto(
                &self
                    .prepare_login_user(&req.collection, resolved.user_doc.clone())
                    .await?,
                &req.collection,
            )),
            mfa_required: None,
            mfa_challenge: None,
            totp_provisioning_uri: None,
        }))
    }
}
