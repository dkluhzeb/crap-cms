//! Delete handler — soft-delete (trash) or permanently delete a document.
//!
//! Codec over [`op::run_blocking`]. `force_hard_delete` is expressed by the
//! operation body via [`op::Operation::adjust_collection_def`] — the
//! definition-clone trick previously copy-pasted on every surface.

use std::sync::Arc;

use tonic::{Request, Response, Status};

use crate::{
    api::{content, handlers::ContentService},
    core::collection::Surface,
    service::op::{self, Credentials, Delete, DeleteArgs, Principal, TargetRef},
};

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Delete a document by ID, running before/after delete hooks.
    ///
    /// Permission check depends on the type of deletion:
    /// - Soft delete (trash): check `access.trash`, falling back to `access.update`
    /// - Permanent delete (`force_hard_delete` or no `soft_delete`): check `access.delete`
    pub(in crate::api::handlers) async fn delete_impl(
        &self,
        request: Request<content::DeleteRequest>,
    ) -> Result<Response<content::DeleteResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let headers = self.metadata_headers(&metadata);
        let req = request.into_inner();
        let def = self.get_collection_def(&req.collection)?;

        let force_hard_delete = req.force_hard_delete;
        let will_soft_delete = def.soft_delete && !force_hard_delete;

        let args = DeleteArgs::builder(req.id.clone())
            .force_hard_delete(force_hard_delete)
            .events(req.events.unwrap_or(true))
            .build();

        let principal = Principal::Credentials(Credentials {
            surface: Surface::Grpc,
            bearer: token,
            session_cookie: None,
            headers,
        });

        op::run_blocking::<Delete>(
            Arc::clone(&self.infra),
            principal,
            TargetRef::collection(req.collection),
            args,
        )
        .await
        .map_err(|e| self.core_error_status(e))?;

        Ok(Response::new(content::DeleteResponse {
            soft_deleted: will_soft_delete,
        }))
    }
}
