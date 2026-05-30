//! Reset password handler — reset password using a valid reset token.

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::core::collection::Auth;
use crate::{
    api::{content, handlers::ContentService},
    core::CollectionDefinition,
    db::DbPool,
    service::{ServiceContext, ServiceError, auth::consume_reset_token},
};

/// Owned bundle for the `ResetPassword` spawn-blocking body.
struct ResetPasswordBlockingInput {
    pool: DbPool,
    slug: String,
    def: CollectionDefinition,
    token: String,
    password: String,
}

/// The outer `Status` carries infrastructure failures (pool/tx/commit) that
/// always map to 500 internal. The inner `Result<(), ServiceError>` carries
/// the semantic outcome of `consume_reset_token` — kept as `ServiceError`
/// so the caller can record a rate-limit failure before mapping to `Status`.
fn reset_password_blocking(
    input: &ResetPasswordBlockingInput,
) -> Result<Result<(), ServiceError>, Status> {
    let mut conn = input
        .pool
        .get()
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

    if let Err(e) = consume_reset_token(&ctx, &input.token, &input.password) {
        return Ok(Err(e));
    }

    tx.commit()
        .inspect_err(|e| error!("Reset password commit error: {}", e))
        .map_err(|_| Status::internal("Internal error"))?;

    Ok(Ok(()))
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

        if self.ip_forgot_password_limiter.is_blocked(&ip) {
            return Err(Status::resource_exhausted(
                "Too many attempts, try again later",
            ));
        }

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

        let input = ResetPasswordBlockingInput {
            pool: self.pool.clone(),
            slug: req.collection.clone(),
            def,
            token: req.token.clone(),
            password: req.new_password.clone(),
        };

        let result = task::spawn_blocking(move || reset_password_blocking(&input))
            .await
            .inspect_err(|e| error!("Reset password task error: {}", e))
            .map_err(|_| Status::internal("Internal error"))??;

        match result {
            Ok(()) => Ok(Response::new(content::ResetPasswordResponse {})),
            Err(e) => {
                self.ip_forgot_password_limiter.record_failure(&ip);
                Err(Status::from(e))
            }
        }
    }
}
