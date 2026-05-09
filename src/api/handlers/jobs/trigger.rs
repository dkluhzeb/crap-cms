//! TriggerJob handler — trigger a job by slug, queuing it for execution.

use std::sync::Arc;

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{content, handlers::ContentService},
    core::{Registry, auth::SharedTokenProvider},
    db::DbPool,
    hooks::HookRunner,
    service::{self, ServiceContext},
};

/// Owned bundle for the `TriggerJob` spawn-blocking body.
struct TriggerJobBlockingInput {
    pool: DbPool,
    hook_runner: HookRunner,
    token_provider: SharedTokenProvider,
    registry: Arc<Registry>,
    data_json: String,
    slug: String,
    token: Option<String>,
}

/// Resolve the auth user, look up the job definition, and queue the job.
/// Synchronous body of [`ContentService::trigger_job_impl`].
fn trigger_job_blocking(input: TriggerJobBlockingInput) -> Result<String, Status> {
    let conn = input
        .pool
        .get()
        .inspect_err(|e| error!("TriggerJob pool error: {}", e))
        .map_err(|_| Status::internal("Internal error"))?;

    let auth_user = ContentService::resolve_auth_user(
        input.token,
        &*input.token_provider,
        &input.registry,
        &conn,
    )?;

    if auth_user.is_none() {
        return Err(Status::unauthenticated("Authentication required"));
    }

    let job_def = input
        .registry
        .get_job(&input.slug)
        .cloned()
        .ok_or_else(|| Status::not_found(format!("Job '{}' not found", input.slug)))?;

    let job_ctx = ServiceContext::slug_only(&input.slug)
        .conn(&conn)
        .runner(&input.hook_runner)
        .user(auth_user.as_ref().map(|u| &u.user_doc))
        .build();

    let job_run = service::jobs::queue_job(
        &job_ctx,
        &service::jobs::QueueJobInput {
            job_def: &job_def,
            data: Some(&input.data_json),
            scheduled_by: "grpc",
        },
    )
    .map_err(Status::from)?;

    Ok(job_run.id)
}

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Trigger a job by slug, queuing it for execution.
    pub(in crate::api::handlers) async fn trigger_job_impl(
        &self,
        request: Request<content::TriggerJobRequest>,
    ) -> Result<Response<content::TriggerJobResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let req = request.into_inner();

        let input = TriggerJobBlockingInput {
            pool: self.pool.clone(),
            hook_runner: self.hook_runner.clone(),
            token_provider: self.token_provider.clone(),
            registry: self.registry.clone(),
            data_json: req.data_json.unwrap_or_else(|| "{}".to_string()),
            slug: req.slug.clone(),
            token,
        };

        let job_id = task::spawn_blocking(move || trigger_job_blocking(input))
            .await
            .inspect_err(|e| error!("TriggerJob task error: {}", e))
            .map_err(|_| Status::internal("Internal error"))??;

        Ok(Response::new(content::TriggerJobResponse { job_id }))
    }
}
