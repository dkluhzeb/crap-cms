//! Verify email handler — verify an email address using a verification token.

use anyhow::Context as _;
use chrono::Utc;
use tokio::task;
use tonic::{Request, Response, Status};
use tracing::{error, warn};

use crate::{
    api::{content, service::ContentService},
    db::query,
};

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Verify an email address using a verification token.
    pub(in crate::api::service) async fn verify_email_impl(
        &self,
        request: Request<content::VerifyEmailRequest>,
    ) -> Result<Response<content::VerifyEmailResponse>, Status> {
        let req = request.into_inner();
        let def = self.get_collection_def(&req.collection)?;

        if !def.is_auth_collection() {
            return Err(Status::invalid_argument(format!(
                "Collection '{}' is not an auth collection",
                req.collection
            )));
        }

        if !def.auth.as_ref().is_some_and(|a| a.verify_email) {
            return Err(Status::invalid_argument(
                "Email verification is not enabled for this collection",
            ));
        }

        let pool = self.pool.clone();
        let slug = req.collection.clone();
        let token = req.token.clone();
        let def_owned = def;

        let found = task::spawn_blocking(move || {
            let mut conn = pool.get().context("DB connection")?;
            let tx = conn.transaction().context("Start transaction")?;

            match query::find_by_verification_token(&tx, &slug, &def_owned, &token)? {
                Some((user, exp)) => {
                    if Utc::now().timestamp() >= exp {
                        if let Err(e) = query::clear_verification_token(&tx, &slug, &user.id) {
                            warn!("Failed to clear expired verification token: {}", e);
                        }

                        tx.commit()?;
                        return Ok(false);
                    }

                    query::mark_verified(&tx, &slug, &user.id)?;
                    tx.commit()?;

                    Ok(true)
                }
                None => Ok(false),
            }
        })
        .await
        .map_err(|e| {
            error!("Verify email task error: {}", e);
            Status::internal("Internal error")
        })?
        .map_err(|e: anyhow::Error| {
            error!("Verify email error: {}", e);
            Status::internal("Internal error")
        })?;

        if !found {
            return Err(Status::not_found("Invalid verification token"));
        }

        Ok(Response::new(content::VerifyEmailResponse {
            success: true,
        }))
    }
}
