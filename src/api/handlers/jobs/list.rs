//! `ListJobs` handler — list all defined jobs.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{content, handlers::ContentService},
    service::{self, AppInfra, ServiceContext},
};

/// Resolve the auth user, reject anonymous callers, and return the set of job
/// slugs whose definitions this caller may see (those whose runs they may
/// read). Keeps `ListJobs` consistent with the run-read access gate.
fn readable_job_slugs_blocking(
    infra: &AppInfra,
    token: Option<&str>,
    headers: &HashMap<String, String>,
) -> Result<HashSet<String>, Status> {
    let conn = infra
        .pool
        .get()
        .inspect_err(|e| error!("ListJobs pool error: {}", e))
        .map_err(|_| Status::internal("Internal error"))?;

    let auth_user = ContentService::resolve_auth_user(
        token,
        headers,
        &*infra.token_provider,
        &infra.hook_runner,
        &infra.registry,
        &conn,
    )?;

    if auth_user.is_none() {
        return Err(Status::unauthenticated("Authentication required"));
    }

    let ctx = ServiceContext::slug_only("")
        .conn(&conn)
        .runner(&infra.hook_runner)
        .user(auth_user.as_ref().map(|u| &u.user_doc))
        .build();

    let allowed =
        service::jobs::readable_job_slugs(&ctx, &conn, &infra.registry).map_err(Status::from)?;

    Ok(allowed.into_iter().collect())
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
        let headers = self.metadata_headers(&metadata);

        let infra = Arc::clone(&self.infra);

        let allowed = task::spawn_blocking(move || {
            readable_job_slugs_blocking(&infra, token.as_deref(), &headers)
        })
        .await
        .inspect_err(|e| error!("ListJobs task error: {}", e))
        .map_err(|_| Status::internal("Internal error"))??;

        let jobs: Vec<content::JobDefinitionInfo> = self
            .infra
            .registry
            .jobs
            .iter()
            .filter(|(slug, _)| allowed.contains(&***slug))
            .map(|(slug, def)| content::JobDefinitionInfo {
                slug: slug.to_string(),
                schedule: def.schedule.clone(),
                queue: def.queue.clone(),
                // Surface the effective retry count: explicit
                // `JobDefinition.retries` wins, else the queue's
                // `[jobs.queues.<queue>] retries`, else `0`. Consumers
                // (admin UI, ops tooling) get the same number that the
                // scheduler actually uses on queue-time.
                retries: def
                    .retries
                    .unwrap_or_else(|| self.queue_retries.get(&def.queue).copied().unwrap_or(0)),
                timeout: def.timeout,
                concurrency: def.concurrency,
                skip_if_running: def.skip_if_running,
                label: def.labels.singular.clone(),
            })
            .collect();

        Ok(Response::new(content::ListJobsResponse { jobs }))
    }
}
