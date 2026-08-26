//! Reset password handler — reset password using a valid reset token.

use std::sync::Arc;

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::core::collection::Auth;
use crate::{
    api::{content, handlers::ContentService},
    core::CollectionDefinition,
    service::{AppInfra, ServiceContext, auth::consume_reset_token},
};

/// Owned bundle for the `ResetPassword` spawn-blocking body. Process-stable
/// dependencies (pool, invalidation transport) come from the shared
/// [`AppInfra`]; the rest is per-call.
struct ResetPasswordBlockingInput {
    infra: Arc<AppInfra>,
    slug: String,
    def: CollectionDefinition,
    token: String,
    password: String,
}

/// Returns `Ok(())` on a committed reset. Infrastructure failures (pool / tx /
/// commit) map to 500 internal; the semantic outcome of `consume_reset_token`
/// (bad / expired token, …) maps through `Status::from(ServiceError)`. The
/// rate-limit attempt is recorded up front by the caller (atomic
/// `check_and_block`), so this body no longer reports failures back for the
/// caller to record — it just produces the response status.
fn reset_password_blocking(input: &ResetPasswordBlockingInput) -> Result<(), Status> {
    let mut conn = input
        .infra
        .pool
        .write()
        .inspect_err(|e| error!("Reset password DB connection error: {}", e))
        .map_err(|_| Status::internal("Internal error"))?;
    // `transaction_immediate()` — same SELECT-then-UPDATE pattern as
    // `verify_email_blocking`: reads the reset-token row, then writes
    // the new password hash. DEFERRED would risk `SQLITE_BUSY_SNAPSHOT`
    // under concurrent writers.
    let tx = conn
        .transaction_immediate()
        .inspect_err(|e| error!("Reset password start transaction error: {}", e))
        .map_err(|_| Status::internal("Internal error"))?;

    let ctx = ServiceContext::collection(&input.slug, &input.def)
        .conn(&tx)
        .build();

    let user_id = consume_reset_token(&ctx, &input.token, &input.password).map_err(Status::from)?;

    tx.commit()
        .inspect_err(|e| error!("Reset password commit error: {}", e))
        .map_err(|_| Status::internal("Internal error"))?;

    // Tear down the user's open live-update streams POST-COMMIT — a rolled-back
    // reset must never tear down a stream for a change that didn't happen.
    ServiceContext::slug_only(&input.slug)
        .invalidation_transport(Some(input.infra.invalidation_transport.clone()))
        .build()
        .publish_user_invalidation(&user_id);

    Ok(())
}

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Reset a password using a valid reset token.
    pub(in crate::api::handlers) async fn reset_password_impl(
        &self,
        request: Request<content::ResetPasswordRequest>,
    ) -> Result<Response<content::ResetPasswordResponse>, Status> {
        let ip = request
            .remote_addr()
            .map_or_else(|| "unknown".to_string(), |a| a.ip().to_string());
        let req = request.into_inner();

        let def = self.get_collection_def(&req.collection)?;

        if !def.is_auth_collection() {
            return Err(Status::invalid_argument(format!(
                "Collection '{}' is not an auth collection",
                req.collection
            )));
        }

        if !def.auth.as_ref().is_some_and(Auth::password_login_enabled) {
            return Err(Status::permission_denied(
                "Local login is disabled for this collection",
            ));
        }

        if let Err(e) = self.password_policy.validate(&req.new_password) {
            return Err(Status::invalid_argument(e.to_string()));
        }

        // Atomically record this attempt against the IP limiter and bail if it
        // is now over threshold — one backend op, closing the check-then-record
        // race the old `is_blocked` + `record_failure` split left open (a burst
        // of concurrent resets could all observe an under-limit count before any
        // recorded). The gate sits AFTER the local validation above so only
        // genuine token-consumption attempts count, and every such attempt
        // counts (the same idiom as login / forgot-password / the admin reset
        // twin). Uses the dedicated forgot-password IP limiter so reset failures
        // don't block legitimate logins from the same IP.
        if self.ip_forgot_password_limiter.check_and_block(&ip) {
            return Err(Status::resource_exhausted(
                "Too many attempts, try again later",
            ));
        }

        let input = ResetPasswordBlockingInput {
            infra: Arc::clone(&self.infra),
            slug: req.collection.clone(),
            def,
            token: req.token.clone(),
            password: req.new_password.clone(),
        };

        task::spawn_blocking(move || reset_password_blocking(&input))
            .await
            .inspect_err(|e| error!("Reset password task error: {}", e))
            .map_err(|_| Status::internal("Internal error"))??;

        Ok(Response::new(content::ResetPasswordResponse {}))
    }
}
