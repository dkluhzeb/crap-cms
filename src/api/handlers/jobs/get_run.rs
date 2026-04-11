//! GetJobRun handler — get details of a specific job run.

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{content, handlers::ContentService},
    service,
};

use super::job_run_to_proto;

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

        let pool = self.pool.clone();
        let token_provider = self.token_provider.clone();
        let registry = self.registry.clone();
        let id = req.id.clone();

        let run = task::spawn_blocking(move || -> Result<_, Status> {
            let conn = pool.get().map_err(|e| {
                error!("GetJobRun pool error: {}", e);
                Status::internal("Internal error")
            })?;

            let auth_user =
                ContentService::resolve_auth_user(token, &*token_provider, &registry, &conn)?;

            if auth_user.is_none() {
                return Err(Status::unauthenticated("Authentication required"));
            }

            service::jobs::get_job_run(&conn, &id)
                .map_err(Status::from)?
                .ok_or_else(|| Status::not_found(format!("Job run '{}' not found", id)))
        })
        .await
        .inspect_err(|e| error!("GetJobRun task error: {}", e))
        .map_err(|_| Status::internal("Internal error"))??;

        Ok(Response::new(job_run_to_proto(&run)))
    }
}
