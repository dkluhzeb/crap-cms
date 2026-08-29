//! Undelete handler — restore a soft-deleted document from trash.
//!
//! Codec over [`op::run_blocking`]. The capability gate (soft-delete
//! required) is enforced at the shared service chokepoint, so every surface
//! agrees.

use std::sync::Arc;

use tonic::{Request, Response, Status};

use crate::{
    api::{
        content,
        handlers::{ContentService, proto::document_to_proto},
    },
    core::collection::Surface,
    service::op::{self, Credentials, Principal, TargetRef, Undelete, UndeleteArgs},
};

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Undelete a soft-deleted document from trash.
    pub(in crate::api::handlers) async fn undelete_impl(
        &self,
        request: Request<content::UndeleteRequest>,
    ) -> Result<Response<content::UndeleteResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let headers = self.metadata_headers(&metadata);
        let req = request.into_inner();

        let args = UndeleteArgs::new(req.id.clone()).events(req.events.unwrap_or(true));

        let principal = Principal::Credentials(Credentials {
            surface: Surface::Grpc,
            bearer: token,
            session_cookie: None,
            headers,
        });

        let doc = op::run_blocking::<Undelete>(
            Arc::clone(&self.infra),
            principal,
            TargetRef::collection(req.collection.clone()),
            args,
        )
        .await
        .map_err(|e| self.core_error_status(e))?;

        Ok(Response::new(content::UndeleteResponse {
            document: Some(document_to_proto(&doc, &req.collection)),
        }))
    }
}
