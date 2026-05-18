//! `ListJobs` handler — list all defined jobs.

use std::{collections::HashMap, sync::Arc};

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{content, handlers::ContentService},
    core::{Registry, SharedTokenProvider},
    db::DbPool,
    hooks::HookRunner,
};

/// Pull a connection, resolve the auth user, and reject anonymous callers.
fn list_jobs_auth_check_blocking(
    pool: &DbPool,
    token_provider: &SharedTokenProvider,
    hook_runner: &HookRunner,
    registry: &Arc<Registry>,
    token: Option<&str>,
    headers: &HashMap<String, String>,
) -> Result<(), Status> {
    let conn = pool
        .get()
        .inspect_err(|e| error!("ListJobs pool error: {}", e))
        .map_err(|_| Status::internal("Internal error"))?;

    let auth_user = ContentService::resolve_auth_user(
        token,
        headers,
        &**token_provider,
        hook_runner,
        registry,
        &conn,
    )?;

    if auth_user.is_none() {
        return Err(Status::unauthenticated("Authentication required"));
    }

    Ok(())
}

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// List all defined jobs and their configuration.
    pub(in crate::api::handlers) async fn list_jobs_impl(
        &self,
        request: Request<content::ListJobsRequest>,
    ) -> Result<Response<content::ListJobsResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let headers = Self::extract_metadata_headers(&metadata);

        let pool = self.pool.clone();
        let token_provider = self.token_provider.clone();
        let hook_runner = self.hook_runner.clone();
        let registry = Arc::clone(&self.registry);

        task::spawn_blocking(move || {
            list_jobs_auth_check_blocking(
                &pool,
                &token_provider,
                &hook_runner,
                &registry,
                token.as_deref(),
                &headers,
            )
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
