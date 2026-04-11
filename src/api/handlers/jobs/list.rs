//! ListJobs handler — list all defined jobs.

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::api::{content, handlers::ContentService};

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// List all defined jobs and their configuration.
    pub(in crate::api::handlers) async fn list_jobs_impl(
        &self,
        request: Request<content::ListJobsRequest>,
    ) -> Result<Response<content::ListJobsResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);

        let pool = self.pool.clone();
        let token_provider = self.token_provider.clone();
        let registry = self.registry.clone();

        task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| {
                error!("ListJobs pool error: {}", e);
                Status::internal("Internal error")
            })?;

            let auth_user =
                ContentService::resolve_auth_user(token, &*token_provider, &registry, &conn)?;

            if auth_user.is_none() {
                return Err(Status::unauthenticated("Authentication required"));
            }

            Ok::<_, Status>(())
        })
        .await
        .inspect_err(|e| error!("ListJobs task error: {}", e))
        .map_err(|_| Status::internal("Internal error"))??;

        let jobs: Vec<content::JobDefinitionInfo> = self
            .registry
            .jobs
            .iter()
            .map(|(slug, def)| content::JobDefinitionInfo {
                slug: slug.to_string(),
                handler: def.handler.clone(),
                schedule: def.schedule.clone(),
                queue: def.queue.clone(),
                retries: def.retries,
                timeout: def.timeout,
                concurrency: def.concurrency,
                skip_if_running: def.skip_if_running,
                label: def.labels.singular.clone(),
            })
            .collect();

        Ok(Response::new(content::ListJobsResponse { jobs }))
    }
}
