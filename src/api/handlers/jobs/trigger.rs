//! `TriggerJob` handler — trigger a job by slug, queuing it for execution.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{content, handlers::ContentService},
    service::{self, AppInfra, ServiceContext},
};

/// Owned bundle for the `TriggerJob` spawn-blocking body. Process-stable
/// dependencies come from the shared [`AppInfra`]; the rest is per-call.
struct TriggerJobBlockingInput {
    infra: Arc<AppInfra>,
    headers: HashMap<String, String>,
    data: String,
    slug: String,
    token: Option<String>,
    /// `None` → use the definition's default priority; `Some(N)` → override.
    priority: Option<i32>,
    /// Seconds before the run becomes claimable. `0` = immediately.
    delay_secs: u64,
    /// Dedup key — see `QueueJobInput::unique_key`.
    unique_key: Option<String>,
    /// Snapshot of `[jobs.queues]` retries by queue name. Used by
    /// `effective_max_attempts` so jobs defined without an explicit
    /// `retries` inherit the operator's queue-level default.
    queue_retries: HashMap<String, u32>,
}

/// Resolve the auth user, look up the job definition, and queue the job.
/// Synchronous body of [`ContentService::trigger_job_impl`].
fn trigger_job_blocking(input: TriggerJobBlockingInput) -> Result<String, Status> {
    let infra = &input.infra;

    let conn = infra
        .pool
        .get()
        .inspect_err(|e| error!("TriggerJob pool error: {}", e))
        .map_err(|_| Status::internal("Internal error"))?;

    let token = input.token;
    let headers = input.headers;

    let auth_user = ContentService::resolve_auth_user(
        token.as_deref(),
        &headers,
        &*infra.token_provider,
        &infra.hook_runner,
        &infra.registry,
        &conn,
    )?;

    if auth_user.is_none() {
        return Err(Status::unauthenticated("Authentication required"));
    }

    let job_def = infra
        .registry
        .get_job(&input.slug)
        .cloned()
        .ok_or_else(|| Status::not_found(format!("Job '{}' not found", input.slug)))?;

    let job_ctx = ServiceContext::slug_only(&input.slug)
        .conn(&conn)
        .runner(&infra.hook_runner)
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
            data: Some(&input.data),
            scheduled_by: "grpc",
            priority: effective_priority,
            queue_retries,
            delay_secs: input.delay_secs,
            unique_key: input.unique_key.as_deref(),
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

        // A negative delay is a caller bug, not "no delay" — reject it
        // rather than silently clamping.
        let delay_secs = match req.delay {
            None => 0,
            Some(d) => u64::try_from(d)
                .map_err(|_| Status::invalid_argument("delay must be >= 0 seconds"))?,
        };

        let input = TriggerJobBlockingInput {
            infra: Arc::clone(&self.infra),
            data: req.data.unwrap_or_else(|| "{}".to_string()),
            slug: req.slug.clone(),
            token,
            headers,
            priority: req.priority,
            delay_secs,
            unique_key: req.unique,
            queue_retries: self.queue_retries.clone(),
        };

        let job_id = task::spawn_blocking(move || trigger_job_blocking(input))
            .await
            .inspect_err(|e| error!("TriggerJob task error: {}", e))
            .map_err(|_| Status::internal("Internal error"))??;

        Ok(Response::new(content::TriggerJobResponse { job_id }))
    }
}
