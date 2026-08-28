//! `RestoreVersion` handler — restore a document to a previous version.
//!
//! Codec over [`op::run_blocking`].

use std::sync::Arc;

use tonic::{Request, Response, Status};

use crate::{
    api::{
        content,
        handlers::{ContentService, proto::document_to_proto},
    },
    core::collection::Surface,
    service::op::{self, Credentials, Principal, RestoreVersion, RestoreVersionArgs, TargetRef},
};

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Restore a document to a previous version.
    pub(in crate::api::handlers) async fn restore_version_impl(
        &self,
        request: Request<content::RestoreVersionRequest>,
    ) -> Result<Response<content::RestoreVersionResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let headers = self.metadata_headers(&metadata);
        let req = request.into_inner();
        let args = RestoreVersionArgs::new(req.document_id.clone(), req.version_id.clone());

        let principal = Principal::Credentials(Credentials {
            surface: Surface::Grpc,
            bearer: token,
            session_cookie: None,
            headers,
        });

        let doc = op::run_blocking::<RestoreVersion>(
            Arc::clone(&self.infra),
            principal,
            TargetRef::collection(req.collection.clone()),
            args,
        )
        .await
        .map_err(|e| self.core_error_status(e))?;

        Ok(Response::new(content::RestoreVersionResponse {
            document: Some(document_to_proto(&doc, &req.collection)),
        }))
    }
}
