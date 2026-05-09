//! Verify email handler — verify an email address using a verification token.

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{content, handlers::ContentService},
    core::CollectionDefinition,
    db::DbPool,
    service::{ServiceContext, auth::consume_verification_token},
};

/// Owned bundle for the `VerifyEmail` spawn-blocking body.
struct VerifyEmailBlockingInput {
    pool: DbPool,
    slug: String,
    def: CollectionDefinition,
    token: String,
}

fn verify_email_blocking(input: VerifyEmailBlockingInput) -> Result<bool, Status> {
    let mut conn = input
        .pool
        .get()
        .inspect_err(|e| error!("Verify email DB connection error: {}", e))
        .map_err(|_| Status::internal("Internal error"))?;
    let tx = conn
        .transaction()
        .inspect_err(|e| error!("Verify email start transaction error: {}", e))
        .map_err(|_| Status::internal("Internal error"))?;

    let ctx = ServiceContext::collection(&input.slug, &input.def)
        .conn(&tx)
        .build();

    let verified = consume_verification_token(&ctx, &input.token).map_err(Status::from)?;
    tx.commit()
        .inspect_err(|e| error!("Verify email commit error: {}", e))
        .map_err(|_| Status::internal("Internal error"))?;

    Ok(verified)
}

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Verify an email address using a verification token.
    pub(in crate::api::handlers) async fn verify_email_impl(
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

        let input = VerifyEmailBlockingInput {
            pool: self.pool.clone(),
            slug: req.collection.clone(),
            def,
            token: req.token.clone(),
        };

        let found = task::spawn_blocking(move || verify_email_blocking(input))
            .await
            .inspect_err(|e| error!("Verify email task error: {}", e))
            .map_err(|_| Status::internal("Internal error"))??;

        if !found {
            return Err(Status::not_found("Invalid verification token"));
        }

        Ok(Response::new(content::VerifyEmailResponse {
            success: true,
        }))
    }
}
