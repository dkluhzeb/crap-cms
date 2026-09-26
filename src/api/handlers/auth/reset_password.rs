//! Reset password handler — reset password using a valid reset token.

use std::sync::Arc;

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{content, handlers::ContentService, request_client_ip},
    core::{CollectionDefinition, collection::Auth, rate_limit::IP_RESET_PASSWORD_KEYSPACE},
    service::{
        AppInfra,
        auth::{PasswordReset, reset_password_with_token},
    },
};

/// Owned bundle for the `ResetPassword` spawn-blocking body. Process-stable
/// dependencies (pool, invalidation transport) come from the shared
/// [`AppInfra`]; the rest is per-call.
struct ResetPasswordBlockingInput {
    infra: Arc<AppInfra>,
    def: Arc<CollectionDefinition>,
    token: String,
    password: String,
}

/// Reset the password in the named collection. The service owns the
/// transaction (committed only on success) and the post-commit live-stream
/// teardown; its refusals (bad / expired token, …) map through
/// `Status::from(ServiceError)`. The rate-limit attempt is recorded up front
/// by the caller (atomic `check_and_block`).
fn reset_password_blocking(input: &ResetPasswordBlockingInput) -> Result<(), Status> {
    let reset = PasswordReset::new(&input.token, &input.password);

    reset_password_with_token(&input.infra, [input.def.as_ref()], &reset).map_err(Status::from)
}

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Reset a password using a valid reset token.
    pub(in crate::api::handlers) async fn reset_password_impl(
        &self,
        request: Request<content::ResetPasswordRequest>,
    ) -> Result<Response<content::ResetPasswordResponse>, Status> {
        let client = request_client_ip(&request, &self.server_config);
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

        if let Err(e) = self.infra.password_policy.validate(&req.new_password) {
            return Err(Status::invalid_argument(e.to_string()));
        }

        // Atomically record this attempt against the IP limiter and bail if it
        // is now over threshold — one backend op, closing the check-then-record
        // race the old `is_blocked` + `record_failure` split left open (a burst
        // of concurrent resets could all observe an under-limit count before any
        // recorded). The gate sits AFTER the local validation above so only
        // genuine token-consumption attempts count, and every such attempt
        // counts (the same idiom as login / forgot-password / the admin reset
        // twin). Uses the reset-token keyspace the admin twin uses, so reset
        // attempts neither block logins nor drain the forgot-password request
        // budget, and switching surfaces buys no fresh budget.
        if self
            .ip_forgot_password_limiter
            .rescoped(IP_RESET_PASSWORD_KEYSPACE)
            .check_and_block_ip(&client)
        {
            return Err(Status::resource_exhausted(
                "Too many attempts, try again later",
            ));
        }

        let input = ResetPasswordBlockingInput {
            infra: Arc::clone(&self.infra),
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
