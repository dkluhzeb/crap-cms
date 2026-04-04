//! Reset password handler — reset password using a valid reset token.

use anyhow::{Context as _, Error as AnyhowError};
use chrono::Utc;
use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{content, service::ContentService},
    core::auth::ResetTokenError,
    db::query,
};

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Reset a password using a valid reset token.
    pub(in crate::api::service) async fn reset_password_impl(
        &self,
        request: Request<content::ResetPasswordRequest>,
    ) -> Result<Response<content::ResetPasswordResponse>, Status> {
        let ip = request
            .remote_addr()
            .map(|a| a.ip().to_string())
            .unwrap_or_else(|| "unknown".to_string());
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

        if def.auth.as_ref().is_some_and(|a| a.disable_local) {
            return Err(Status::permission_denied(
                "Local login is disabled for this collection",
            ));
        }

        if let Err(e) = self.password_policy.validate(&req.new_password) {
            return Err(Status::invalid_argument(e.to_string()));
        }

        let pool = self.pool.clone();
        let slug = req.collection.clone();
        let token = req.token.clone();
        let password = req.new_password.clone();
        let def_owned = def;

        let result: Result<(), AnyhowError> = task::spawn_blocking(move || {
            let mut conn = pool.get().context("DB connection")?;
            let tx = conn.transaction().context("Start transaction")?;

            let (user, exp) = query::find_by_reset_token(&tx, &slug, &def_owned, &token)?
                .ok_or(ResetTokenError::NotFound)?;

            if query::is_locked(&tx, &slug, &user.id)? {
                query::clear_reset_token(&tx, &slug, &user.id)?;
                tx.commit()?;
                return Err(ResetTokenError::NotFound.into());
            }

            if Utc::now().timestamp() >= exp {
                query::clear_reset_token(&tx, &slug, &user.id)?;
                tx.commit()?;
                return Err(ResetTokenError::Expired.into());
            }

            query::update_password(&tx, &slug, &user.id, &password)?;
            query::clear_reset_token(&tx, &slug, &user.id)?;
            tx.commit()?;

            Ok(())
        })
        .await
        .map_err(|e| {
            error!("Reset password task error: {}", e);
            Status::internal("Internal error")
        })?;

        match result {
            Ok(()) => Ok(Response::new(content::ResetPasswordResponse {
                success: true,
            })),
            Err(e) => {
                self.ip_forgot_password_limiter.record_failure(&ip);

                match e.downcast_ref::<ResetTokenError>() {
                    Some(_) => Err(Status::invalid_argument(e.to_string())),
                    None => {
                        error!("Reset password error: {}", e);
                        Err(Status::internal("Internal error"))
                    }
                }
            }
        }
    }
}
