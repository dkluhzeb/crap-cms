//! `CancelJobRun` handler — cancel one pending job run.

use std::{collections::HashMap, sync::Arc};

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{content, handlers::ContentService},
    service::{self, AppInfra, ServiceContext},
};

/// Owned bundle for the `CancelJobRun` spawn-blocking body.
struct CancelJobRunBlockingInput {
    infra: Arc<AppInfra>,
    token: Option<String>,
    headers: HashMap<String, String>,
    id: String,
}

fn cancel_job_run_blocking(input: &CancelJobRunBlockingInput) -> Result<bool, Status> {
    let infra = &input.infra;

    let conn = infra
        .pool
        .get()
        .inspect_err(|e| error!("CancelJobRun pool error: {}", e))
        .map_err(|_| Status::internal("Internal error"))?;

    let auth_user = ContentService::resolve_auth_user(
        input.token.as_deref(),
        &input.headers,
        &*infra.token_provider,
        &infra.hook_runner,
        &infra.registry,
        &conn,
    )?;

    if auth_user.is_none() {
        return Err(Status::unauthenticated("Authentication required"));
    }

    // The run's own slug drives the access gate, so the context slug is
    // irrelevant here — same shape as `GetJobRun`.
    let ctx = ServiceContext::slug_only("")
        .conn(&conn)
        .runner(&infra.hook_runner)
        .user(auth_user.as_ref().map(|u| &u.user_doc))
        .build();

    service::jobs::cancel_job_run(&ctx, infra.registry.as_ref(), &input.id)
        .map_err(|e| Status::internal(e.to_string()))
}

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Cancel one pending job run (a queued bulk operation, or a user job's
    /// queued run), authorized exactly like reading it.
    pub(in crate::api::handlers) async fn cancel_job_run_impl(
        &self,
        request: Request<content::CancelJobRunRequest>,
    ) -> Result<Response<content::CancelJobRunResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let headers = self.metadata_headers(&metadata);
        let req = request.into_inner();

        let input = CancelJobRunBlockingInput {
            infra: Arc::clone(&self.infra),
            token,
            headers,
            id: req.id,
        };

        let cancelled = task::spawn_blocking(move || cancel_job_run_blocking(&input))
            .await
            .inspect_err(|e| error!("CancelJobRun task error: {e}"))
            .map_err(|_| Status::internal("Internal error"))??;

        Ok(Response::new(content::CancelJobRunResponse { cancelled }))
    }
}
