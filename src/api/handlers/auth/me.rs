//! Me handler — return the currently authenticated user.

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{
        content,
        handlers::{ContentService, proto::document_to_proto},
    },
    core::{CollectionDefinition, Document},
    db::{DbPool, query},
    service::{self, ServiceContext},
};

/// Owned bundle for the `Me` spawn-blocking body.
struct MeBlockingInput {
    pool: DbPool,
    collection: String,
    id: String,
    def: CollectionDefinition,
}

fn me_blocking(input: MeBlockingInput) -> Result<(Option<Document>, u64, bool), Status> {
    let conn = input
        .pool
        .get()
        .inspect_err(|e| error!("Me DB connection error: {}", e))
        .map_err(|_| Status::internal("Internal error"))?;

    // Auth infrastructure — direct query for user lookup, not a user-facing read.
    let mut doc = query::find_by_id(&conn, &input.collection, &input.def, &input.id, None)
        .inspect_err(|e| error!("Me find_by_id error: {}", e))
        .map_err(|_| Status::internal("Internal error"))?;

    if let Some(ref mut d) = doc {
        query::hydrate_document(&conn, &input.collection, &input.def.fields, d, None, None)
            .inspect_err(|e| error!("Me hydrate_document error: {}", e))
            .map_err(|_| Status::internal("Internal error"))?;
    }

    let ctx = ServiceContext::slug_only(&input.collection)
        .conn(&conn)
        .build();
    let sv = service::auth::get_session_version(&ctx, &input.id).map_err(Status::from)?;
    let locked = service::auth::is_locked(&ctx, &input.id).map_err(Status::from)?;

    Ok((doc, sv, locked))
}

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Return the currently authenticated user from a JWT token.
    pub(in crate::api::handlers) async fn me_impl(
        &self,
        request: Request<content::MeRequest>,
    ) -> Result<Response<content::MeResponse>, Status> {
        let metadata = request.metadata().clone();
        let req = request.into_inner();

        let token = Self::extract_token(&metadata)
            .or_else(|| {
                let t = &req.token;
                if t.is_empty() { None } else { Some(t.clone()) }
            })
            .ok_or_else(|| Status::unauthenticated("Missing token"))?;

        let claims = self
            .token_provider
            .validate_token(&token)
            .map_err(|_| Status::unauthenticated("Invalid or expired token"))?;

        let def = self.get_collection_def(&claims.collection)?;

        let input = MeBlockingInput {
            pool: self.pool.clone(),
            collection: claims.collection.to_string(),
            id: claims.sub.to_string(),
            def,
        };
        let session_version = claims.session_version;

        let (doc, db_session_version, is_locked) = task::spawn_blocking(move || me_blocking(input))
            .await
            .inspect_err(|e| error!("Me task error: {}", e))
            .map_err(|_| Status::internal("Internal error"))??;

        let doc = doc.ok_or_else(|| Status::not_found("User not found"))?;

        if session_version != db_session_version {
            return Err(Status::unauthenticated("Session invalidated"));
        }

        if is_locked {
            return Err(Status::unauthenticated("Account is locked"));
        }

        Ok(Response::new(content::MeResponse {
            user: Some(document_to_proto(&doc, &claims.collection)),
        }))
    }
}
