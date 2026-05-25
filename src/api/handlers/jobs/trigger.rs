//! `TriggerJob` handler — trigger a job by slug, queuing it for execution.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{content, handlers::ContentService},
    core::{Registry, SharedTokenProvider},
    db::DbPool,
    hooks::HookRunner,
    service::{self, ServiceContext},
};

/// Owned bundle for the `TriggerJob` spawn-blocking body.
struct TriggerJobBlockingInput {
    pool: DbPool,
    hook_runner: HookRunner,
    headers: HashMap<String, String>,
    token_provider: SharedTokenProvider,
    registry: Arc<Registry>,
    data_json: String,
    slug: String,
    token: Option<String>,
    /// `None` → use the definition's default priority; `Some(N)` → override.
    priority: Option<i32>,
    /// Snapshot of `[jobs.queues]` retries by queue name. Used by
    /// `effective_max_attempts` so jobs defined without an explicit
    /// `retries` inherit the operator's queue-level default.
    queue_retries: HashMap<String, u32>,
}

/// Resolve the auth user, look up the job definition, and queue the job.
/// Synchronous body of [`ContentService::trigger_job_impl`].
fn trigger_job_blocking(input: TriggerJobBlockingInput) -> Result<String, Status> {
    let conn = input
        .pool
        .get()
        .inspect_err(|e| error!("TriggerJob pool error: {}", e))
        .map_err(|_| Status::internal("Internal error"))?;

    let token = input.token;
    let headers = input.headers;

    let auth_user = ContentService::resolve_auth_user(
        token.as_deref(),
        &headers,
        &*input.token_provider,
        &input.hook_runner,
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

    // Per-call gRPC priority overrides the definition's default;
    // absent → fall back to `JobDefinition::priority`.
    let effective_priority = input.priority.unwrap_or(job_def.priority);

    let queue_retries = input.queue_retries.get(&job_def.queue).copied();

    let job_run = service::jobs::queue_job(
        &job_ctx,
        &service::jobs::QueueJobInput {
            job_def: &job_def,
            data: Some(&input.data_json),
            scheduled_by: "grpc",
            priority: effective_priority,
            queue_retries,
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
        let headers = self.metadata_headers(&metadata);
        let req = request.into_inner();

        let input = TriggerJobBlockingInput {
            pool: self.pool.clone(),
            hook_runner: self.hook_runner.clone(),
            token_provider: self.token_provider.clone(),
            registry: Arc::clone(&self.registry),
            data_json: req.data_json.unwrap_or_else(|| "{}".to_string()),
            slug: req.slug.clone(),
            token,
            headers,
            priority: req.priority,
            queue_retries: self.queue_retries.clone(),
        };

        let job_id = task::spawn_blocking(move || trigger_job_blocking(input))
            .await
            .inspect_err(|e| error!("TriggerJob task error: {}", e))
            .map_err(|_| Status::internal("Internal error"))??;

        Ok(Response::new(content::TriggerJobResponse { job_id }))
    }
}
