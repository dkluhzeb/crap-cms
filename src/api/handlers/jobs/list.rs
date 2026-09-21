//! `ListJobs` handler — list all defined jobs.

use std::{collections::HashMap, sync::Arc};

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{
        content,
        handlers::{ContentService, content_service::pool_error_status},
    },
    service::{self, AppInfra, ServiceContext, jobs::JobDefinitionInfo},
};

/// Project one described job onto the wire message. A field added to
/// [`JobDefinitionInfo`] shows up here as a missing-field compile error, which is the
/// point: the wire cannot quietly describe less than the other surfaces.
fn job_definition_wire(job: JobDefinitionInfo) -> content::JobDefinitionInfo {
    content::JobDefinitionInfo {
        slug: job.slug,
        schedule: job.schedule,
        queue: job.queue,
        retries: job.retries,
        timeout: job.timeout,
        concurrency: job.concurrency,
        skip_if_running: job.skip_if_running,
        label: job.label,
        priority: job.priority,
    }
}

/// Resolve the auth user, reject anonymous callers, and describe the jobs
/// this caller may see (those whose runs they may read). Keeps `ListJobs`
/// consistent with the run-read access gate, and the description itself with
/// every other surface that lists jobs.
fn list_jobs_blocking(
    infra: &AppInfra,
    token: Option<&str>,
    headers: &HashMap<String, String>,
    queue_retries: &HashMap<String, u32>,
) -> Result<Vec<JobDefinitionInfo>, Status> {
    let kind = infra.pool.kind();
    let conn = infra
        .pool
        .get()
        .inspect_err(|e| error!("ListJobs pool error: {}", e))
        .map_err(|e| pool_error_status(e, kind))?;

    let auth_user = ContentService::resolve_auth_user(
        token,
        headers,
        &*infra.token_provider,
        &infra.hook_runner,
        &infra.registry,
        &conn,
        &infra.locale_config,
    )?;

    if auth_user.is_none() {
        return Err(Status::unauthenticated("Authentication required"));
    }

    let ctx = ServiceContext::slug_only("")
        .conn(&conn)
        .runner(&infra.hook_runner)
        .user(auth_user.as_ref().map(|u| &u.user_doc))
        .build();

    service::jobs::list_jobs(&ctx, &conn, &infra.registry, queue_retries)
        .map_err(|e| Status::from(e.reclassify(infra.pool.kind())))
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
        let queue_retries = self.queue_retries.clone();

        let jobs = task::spawn_blocking(move || {
            list_jobs_blocking(&infra, token.as_deref(), &headers, &queue_retries)
        })
        .await
        .inspect_err(|e| error!("ListJobs task error: {}", e))
        .map_err(|_| Status::internal("Internal error"))??;

        let jobs: Vec<content::JobDefinitionInfo> =
            jobs.into_iter().map(job_definition_wire).collect();

        Ok(Response::new(content::ListJobsResponse { jobs }))
    }
}
