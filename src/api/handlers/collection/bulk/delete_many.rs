//! Bulk `DeleteMany` RPC handler.
//!
//! Codec over [`op::run_blocking`]. `force_hard_delete` and the post-commit
//! upload-file cleanup live in the operation body.

use std::sync::Arc;

use tonic::{Request, Response, Status};

use crate::{
    api::{
        content,
        handlers::{ContentService, collection::filter_builder::decode_where_json},
    },
    core::collection::Surface,
    service::op::{self, Credentials, DeleteMany, DeleteManyArgs, Principal, TargetRef},
};

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Bulk delete matching documents. Runs per-document lifecycle hooks by default.
    pub(in crate::api::handlers) async fn delete_many_impl(
        &self,
        request: Request<content::DeleteManyRequest>,
    ) -> Result<Response<content::DeleteManyResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let headers = self.metadata_headers(&metadata);
        let req = request.into_inner();

        let filters = decode_where_json(req.r#where.as_deref())?;

        let args = DeleteManyArgs::builder(filters)
            .run_hooks(req.hooks.unwrap_or(true))
            .force_hard_delete(req.force_hard_delete)
            .max_documents(self.server_config.bulk_max_documents)
            .events(req.events.unwrap_or(false))
            .build();

        let principal = Principal::Credentials(Credentials {
            surface: Surface::Grpc,
            bearer: token,
            session_cookie: None,
            headers,
        });

        let result = op::run_blocking::<DeleteMany>(
            Arc::clone(&self.infra),
            principal,
            TargetRef::collection(req.collection),
            args,
        )
        .await
        .map_err(|e| self.core_error_status(e))?;

        Ok(Response::new(content::DeleteManyResponse {
            deleted: result.hard_deleted,
            soft_deleted: result.soft_deleted,
            skipped: result.skipped,
        }))
    }
}
