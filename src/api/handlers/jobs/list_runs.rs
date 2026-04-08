//! ListJobRuns handler — list job runs with optional filters.

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{content, handlers::ContentService},
    db::query::jobs,
};

use super::job_run_to_proto;

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// List job runs with optional filters.
    pub(in crate::api::handlers) async fn list_job_runs_impl(
        &self,
        request: Request<content::ListJobRunsRequest>,
    ) -> Result<Response<content::ListJobRunsResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let req = request.into_inner();

        let pool = self.pool.clone();
        let token_provider = self.token_provider.clone();
        let registry = self.registry.clone();
        let slug = req.slug.clone();
        let status = req.status.clone();
        let limit = req.limit.unwrap_or(50).min(1000);
        let offset = req.offset.unwrap_or(0);

        let runs = task::spawn_blocking(move || -> Result<_, Status> {
            let conn = pool.get().map_err(|e| {
                error!("ListJobRuns pool error: {}", e);
                Status::internal("Internal error")
            })?;

            let auth_user =
                ContentService::resolve_auth_user(token, &*token_provider, &registry, &conn)?;

            if auth_user.is_none() {
                return Err(Status::unauthenticated("Authentication required"));
            }

            jobs::list_job_runs(&conn, slug.as_deref(), status.as_deref(), limit, offset).map_err(
                |e| {
                    error!("ListJobRuns query error: {}", e);
                    Status::internal("Internal error")
                },
            )
        })
        .await
        .inspect_err(|e| error!("ListJobRuns task error: {}", e))
        .map_err(|_| Status::internal("Internal error"))??;

        let runs: Vec<content::GetJobRunResponse> = runs.iter().map(job_run_to_proto).collect();

        Ok(Response::new(content::ListJobRunsResponse { runs }))
    }
}
