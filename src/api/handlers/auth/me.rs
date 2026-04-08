//! Me handler — return the currently authenticated user.

use anyhow::{Context as _, Error as AnyhowError};
use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{
        content,
        handlers::{ContentService, convert::document_to_proto},
    },
    db::query,
};

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

        let pool = self.pool.clone();
        let collection = claims.collection.clone();
        let id = claims.sub.clone();
        let session_version = claims.session_version;

        let (doc, db_session_version, is_locked) = task::spawn_blocking(move || {
            let conn = pool.get().context("DB connection")?;
            let mut doc = query::find_by_id(&conn, &collection, &def, &id, None)?;

            if let Some(ref mut d) = doc {
                query::hydrate_document(&conn, &collection, &def.fields, d, None, None)?;
            }

            let sv = query::get_session_version(&conn, &collection, &id)?;
            let locked = query::is_locked(&conn, &collection, &id)?;

            Ok::<_, AnyhowError>((doc, sv, locked))
        })
        .await
        .inspect_err(|e| error!("Me task error: {}", e))
        .map_err(|_| Status::internal("Internal error"))?
        .map_err(|e| {
            error!("Me query error: {}", e);
            Status::internal("Internal error")
        })?;

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
