//! `ListJobRuns` handler — list job runs with optional filters.

use std::{collections::HashMap, sync::Arc};

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::handlers::proto::pagination_result_to_proto,
    api::{content, handlers::ContentService},
    core::{JobRun, Registry, SharedTokenProvider},
    db::DbPool,
    hooks::HookRunner,
    service::{self, PaginatedResult},
};

use super::job_run_to_proto;

/// Owned bundle for the `ListJobRuns` spawn-blocking body.
struct ListJobRunsBlockingInput {
    pool: DbPool,
    token_provider: SharedTokenProvider,
    hook_runner: HookRunner,
    registry: Arc<Registry>,
    token: Option<String>,
    headers: HashMap<String, String>,
    slug: Option<String>,
    status: Option<String>,
    limit: i64,
    offset: i64,
}

/// Resolve the auth user and run the `list_job_runs` service call.
fn list_job_runs_blocking(
    input: ListJobRunsBlockingInput,
) -> Result<PaginatedResult<JobRun>, Status> {
    let conn = input
        .pool
        .get()
        .inspect_err(|e| error!("ListJobRuns pool error: {}", e))
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

    service::jobs::list_job_runs(
        &conn,
        &service::jobs::ListJobRunsInput {
            slug: input.slug.as_deref(),
            status: input.status.as_deref(),
            limit: input.limit,
            offset: input.offset,
        },
    )
    .map_err(Status::from)
}

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// List job runs with optional filters.
    pub(in crate::api::handlers) async fn list_job_runs_impl(
        &self,
        request: Request<content::ListJobRunsRequest>,
    ) -> Result<Response<content::ListJobRunsResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let headers = self.metadata_headers(&metadata);
        let req = request.into_inner();

        let input = ListJobRunsBlockingInput {
            pool: self.pool.clone(),
            token_provider: self.token_provider.clone(),
            hook_runner: self.hook_runner.clone(),
            registry: Arc::clone(&self.registry),
            token,
            headers,
            slug: req.slug.clone(),
            status: req.status.clone(),
            limit: req.limit.unwrap_or(50).min(1000),
            offset: req.offset.unwrap_or(0).max(0),
        };

        let runs = task::spawn_blocking(move || list_job_runs_blocking(input))
            .await
            .inspect_err(|e| error!("ListJobRuns task error: {}", e))
            .map_err(|_| Status::internal("Internal error"))??;

        let pagination = pagination_result_to_proto(&runs.pagination);
        let proto_runs: Vec<content::GetJobRunResponse> =
            runs.docs.iter().map(job_run_to_proto).collect();

        Ok(Response::new(content::ListJobRunsResponse {
            runs: proto_runs,
            pagination: Some(pagination),
        }))
    }
}
