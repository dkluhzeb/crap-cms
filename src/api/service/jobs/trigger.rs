//! TriggerJob handler — trigger a job by slug, queuing it for execution.

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{content, service::ContentService},
    db::{AccessResult, query::jobs},
};

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Trigger a job by slug, queuing it for execution.
    pub(in crate::api::service) async fn trigger_job_impl(
        &self,
        request: Request<content::TriggerJobRequest>,
    ) -> Result<Response<content::TriggerJobResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let req = request.into_inner();

        let pool = self.pool.clone();
        let hook_runner = self.hook_runner.clone();
        let token_provider = self.token_provider.clone();
        let registry = self.registry.clone();
        let data_json = req.data_json.unwrap_or_else(|| "{}".to_string());
        let slug = req.slug.clone();

        let job_id = task::spawn_blocking(move || -> Result<String, Status> {
            let mut conn = pool.get().map_err(|e| {
                error!("TriggerJob pool error: {}", e);
                Status::internal("Internal error")
            })?;

            let auth_user =
                ContentService::resolve_auth_user(token, &*token_provider, &registry, &conn)?;

            if auth_user.is_none() {
                return Err(Status::unauthenticated("Authentication required"));
            }

            let job_def = registry
                .get_job(&slug)
                .cloned()
                .ok_or_else(|| Status::not_found(format!("Job '{}' not found", slug)))?;

            if job_def.access.is_some() {
                let result = ContentService::check_access_blocking(
                    job_def.access.as_deref(),
                    &auth_user,
                    None,
                    None,
                    &hook_runner,
                    &mut conn,
                )?;

                if matches!(result, AccessResult::Denied) {
                    return Err(Status::permission_denied("Trigger access denied"));
                }
            }

            let job_run = jobs::insert_job(
                &conn,
                &slug,
                &data_json,
                "grpc",
                job_def.retries + 1,
                &job_def.queue,
            )
            .map_err(|e| {
                error!("Failed to queue job: {}", e);
                Status::internal("Internal error")
            })?;

            Ok(job_run.id)
        })
        .await
        .inspect_err(|e| error!("TriggerJob task error: {}", e))
        .map_err(|_| Status::internal("Internal error"))??;

        Ok(Response::new(content::TriggerJobResponse { job_id }))
    }
}
