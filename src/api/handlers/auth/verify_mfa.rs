//! `VerifyMfa` handler — complete an MFA-gated login.
//!
//! Counterpart of the admin `/admin/mfa` action over the same chokepoints:
//! the pending token minted by `Login` (purpose-bound `MfaPending` claims, so
//! a session token can't be replayed here and this token can't be used as a
//! session), the stored single-use 6-digit code, and the shared `mfa` /
//! `ip_mfa` guess limiters.

use std::sync::Arc;

use chrono::Utc;
use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{
        content,
        handlers::{ContentService, proto::document_to_proto},
    },
    core::{Slug, auth::ClaimsBuilder},
    db::DbPool,
    service::{AppInfra, ServiceContext, auth},
};

/// Verify the code against the stored (single-use, expiring) MFA code.
fn verify_code_blocking(
    pool: &DbPool,
    slug: &str,
    user_id: &str,
    code: &str,
) -> anyhow::Result<bool> {
    let conn = pool.get()?;
    let ctx = ServiceContext::slug_only(slug).conn(&conn).build();

    auth::verify_mfa_code(&ctx, user_id, code).map_err(crate::service::ServiceError::into_anyhow)
}

/// Re-load the user post-verification. Fails CLOSED: a user that was locked,
/// deleted, or session-invalidated inside the pending window must not
/// complete the login.
fn load_user_blocking(
    infra: &Arc<AppInfra>,
    claims: &crate::core::auth::Claims,
) -> Option<crate::core::AuthUser> {
    let conn = infra.pool.get().ok()?;

    auth::load_authenticated_user(claims, &infra.registry, &conn)
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
        // MfaPending, expiring with the 5-minute window.
        let Ok(pending) = self
            .infra
            .token_provider
            .validate_pending_token(&req.mfa_challenge)
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

        let pool = self.infra.pool.clone();
        let slug = req.collection.clone();
        let code = req.code.clone();
        let uid = user_id.clone();

        let verified =
            task::spawn_blocking(move || verify_code_blocking(&pool, &slug, &uid, &code))
                .await
                .inspect_err(|e| error!("VerifyMfa task error: {e}"))
                .map_err(|_| Status::internal("Internal error"))?
                .inspect_err(|e| error!("VerifyMfa error: {e:#}"))
                .map_err(|_| Status::internal("Internal error"))?;

        if !verified {
            return Err(Status::unauthenticated("Invalid MFA code"));
        }

        // Re-resolve the user fail-closed (lock/delete/session bump inside
        // the pending window invalidates the challenge).
        let infra = Arc::clone(&self.infra);
        let claims_for_load = pending.clone();

        let Some(resolved) =
            task::spawn_blocking(move || load_user_blocking(&infra, &claims_for_load))
                .await
                .inspect_err(|e| error!("VerifyMfa load task error: {e}"))
                .map_err(|_| Status::internal("Internal error"))?
        else {
            return Err(Status::unauthenticated("Invalid or expired MFA challenge"));
        };

        let def = self.get_collection_def(&req.collection)?;
        let expiry = def.auth.as_ref().map_or(7200, |a| a.token_expiry);
        let now = Utc::now().timestamp().max(0).cast_unsigned();

        let claims = ClaimsBuilder::new(pending.sub.clone(), Slug::new(&req.collection))
            .email(pending.email.clone())
            .exp(now.saturating_add(expiry))
            .auth_time(now)
            .session_version(pending.session_version)
            .build()
            .inspect_err(|e| error!("Claims build error: {e}"))
            .map_err(|_| Status::internal("Internal error"))?;

        let token = self
            .infra
            .token_provider
            .create_token(&claims)
            .inspect_err(|e| error!("Token creation error: {e}"))
            .map_err(|_| Status::internal("Internal error"))?;

        // This identity just completed its second factor — clear its guess
        // budget and refund the shared per-IP attempt (mirrors the login
        // limiter semantics: a success must not wipe other identities'
        // failures from the same IP).
        self.mfa_limiter.clear(&user_id);
        self.ip_mfa_limiter.refund(&ip);

        Ok(Response::new(content::LoginResponse {
            token,
            user: Some(document_to_proto(&resolved.user_doc, &req.collection)),
            mfa_required: None,
            mfa_challenge: None,
        }))
    }
}
