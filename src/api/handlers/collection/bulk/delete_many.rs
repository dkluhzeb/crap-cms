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
    service::{
        jobs::bulk_queue::{BulkJobData, BulkOpKind, QueuedBy},
        op::{self, Credentials, DeleteMany, DeleteManyArgs, Principal, TargetRef},
    },
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

        let principal = Principal::Credentials(Credentials {
            surface: Surface::Grpc,
            bearer: token,
            session_cookie: None,
            headers,
        });

        // Validate the where clause up front either way.
        let filters = decode_where_json(req.r#where.as_deref())?;

        if req.queue.unwrap_or(false) {
            let job = BulkJobData {
                op: BulkOpKind::DeleteMany,
                collection: req.collection.clone(),
                queued_by: QueuedBy::System, // stamped for real in queue_bulk_run
                ui_locale: None,             // ditto
                locale: None,
                draft: false,
                hooks: req.hooks.unwrap_or(true),
                events: req.events.unwrap_or(false),
                max_documents: self.server_config.bulk_max_documents,
                documents: None,
                where_clause: req.r#where.clone(),
                data: None,
                force_hard_delete: req.force_hard_delete,
            };

            let job_id = self.queue_bulk_run(principal, job).await?;

            return Ok(Response::new(content::DeleteManyResponse {
                deleted: 0,
                soft_deleted: 0,
                skipped: 0,
                job_id: Some(job_id),
            }));
        }

        let args = DeleteManyArgs::builder(filters)
            .run_hooks(req.hooks.unwrap_or(true))
            .force_hard_delete(req.force_hard_delete)
            .max_documents(self.server_config.bulk_max_documents)
            .events(req.events.unwrap_or(false))
            .build();

        let result = op::run_blocking::<DeleteMany>(
            Arc::clone(&self.infra),
            principal,
            TargetRef::collection(req.collection),
            args,
        )
        .await
        .map_err(|e| self.core_error_status(e))?;

        Ok(Response::new(content::DeleteManyResponse {
            job_id: None,
            deleted: result.hard_deleted,
            soft_deleted: result.soft_deleted,
            skipped: result.skipped,
        }))
    }
}
