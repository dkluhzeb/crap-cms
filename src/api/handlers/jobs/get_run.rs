//! GetJobRun handler — get details of a specific job run.

use std::sync::Arc;

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{content, handlers::ContentService},
    core::{JobRun, Registry, auth::SharedTokenProvider},
    db::DbPool,
    service,
};

use super::job_run_to_proto;

/// Owned bundle for the `GetJobRun` spawn-blocking body.
struct GetJobRunBlockingInput {
    pool: DbPool,
    token_provider: SharedTokenProvider,
    registry: Arc<Registry>,
    token: Option<String>,
    id: String,
}

fn get_job_run_blocking(input: GetJobRunBlockingInput) -> Result<JobRun, Status> {
    let conn = input
        .pool
        .get()
        .inspect_err(|e| error!("GetJobRun pool error: {}", e))
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

    service::jobs::get_job_run(&conn, &input.id)
        .map_err(Status::from)?
        .ok_or_else(|| Status::not_found(format!("Job run '{}' not found", input.id)))
}

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Get details of a specific job run.
    pub(in crate::api::handlers) async fn get_job_run_impl(
        &self,
        request: Request<content::GetJobRunRequest>,
    ) -> Result<Response<content::GetJobRunResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let req = request.into_inner();

        let input = GetJobRunBlockingInput {
            pool: self.pool.clone(),
            token_provider: self.token_provider.clone(),
            registry: self.registry.clone(),
            token,
            id: req.id.clone(),
        };

        let run = task::spawn_blocking(move || get_job_run_blocking(input))
            .await
            .inspect_err(|e| error!("GetJobRun task error: {}", e))
            .map_err(|_| Status::internal("Internal error"))??;

        Ok(Response::new(job_run_to_proto(&run)))
    }
}
