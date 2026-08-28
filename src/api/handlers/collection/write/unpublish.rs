//! Unpublish handler — revert a published document to draft status.
//!
//! Codec over [`op::run_blocking`], invoked from the update handler when
//! `unpublish = true`.

use std::collections::HashMap;
use std::sync::Arc;

use tonic::{Response, Status};

use crate::{
    api::{
        content,
        handlers::{ContentService, proto::document_to_proto},
    },
    core::collection::Surface,
    service::op::{self, Credentials, Principal, TargetRef, Unpublish, UnpublishArgs},
};

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Unpublish a document: set status to draft, create version snapshot.
    pub(in crate::api::handlers) async fn unpublish_impl(
        &self,
        token: Option<String>,
        headers: HashMap<String, String>,
        req: &content::UpdateRequest,
    ) -> Result<Response<content::UpdateResponse>, Status> {
        let args = UnpublishArgs::new(req.id.clone()).events(req.events.unwrap_or(true));

        let principal = Principal::Credentials(Credentials {
            surface: Surface::Grpc,
            bearer: token,
            session_cookie: None,
            headers,
        });

        let doc = op::run_blocking::<Unpublish>(
            Arc::clone(&self.infra),
            principal,
            TargetRef::collection(req.collection.clone()),
            args,
        )
        .await
        .map_err(|e| self.core_error_status(e))?;

        Ok(Response::new(content::UpdateResponse {
            document: Some(document_to_proto(&doc, &req.collection)),
        }))
    }
}
