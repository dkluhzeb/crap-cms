//! Schema/metadata RPC handlers: GetGlobal, UpdateGlobal, ListCollections,
//! DescribeCollection, Subscribe, ListVersions, RestoreVersion, ListJobs,
//! TriggerJob, GetJobRun, ListJobRuns.

use anyhow::Context as _;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::pin::Pin;
use tokio_stream::{wrappers::BroadcastStream, Stream, StreamExt};
use tonic::{Request, Response, Status};

use crate::api::content;
use crate::db::query::{AccessResult, LocaleContext};
use crate::db::{ops, query};

use super::convert::{
    document_to_proto, field_def_to_proto, json_to_prost_value,
    prost_struct_to_hashmap, prost_struct_to_json_map,
};
use super::ContentService;

/// Untestable as unit: async methods require full ContentService with pool, registry,
/// hook_runner, and JWT secret. Covered by integration tests in tests/ directory.
#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Get the single document for a global definition.
    pub(super) async fn get_global_impl(
        &self,
        request: Request<content::GetGlobalRequest>,
    ) -> Result<Response<content::GetGlobalResponse>, Status> {
        let metadata = request.metadata().clone();
        let auth_user = self.extract_auth_user(&metadata);
        let req = request.into_inner();
        let def = self.get_global_def(&req.slug)?;

        // Check read access
        let access_result =
            self.require_access(def.access.read.as_deref(), &auth_user, None, None)?;
        if matches!(access_result, AccessResult::Denied) {
            return Err(Status::permission_denied("Read access denied"));
        }

        let locale_ctx =
            LocaleContext::from_locale_string(req.locale.as_deref(), &self.locale_config);

        let pool = self.pool.clone();
        let runner = self.hook_runner.clone();
        let hooks = def.hooks.clone();
        let def_fields = def.fields.clone();
        let fields = def_fields.clone();
        let slug = req.slug.clone();
        let doc = tokio::task::spawn_blocking(move || {
            runner.fire_before_read(&hooks, &slug, "get_global", HashMap::new())?;
            let doc = ops::get_global(&pool, &slug, &def, locale_ctx.as_ref())?;
            let doc = runner.apply_after_read(&hooks, &fields, &slug, "get_global", doc);
            Ok::<_, anyhow::Error>(doc)
        })
        .await
        .map_err(|e| Status::internal(format!("Task error: {}", e)))?
        .map_err(|e| Status::internal(format!("Query error: {}", e)))?;

        let mut proto_doc = document_to_proto(&doc, &req.slug);
        self.strip_denied_read_fields(&mut proto_doc, &def_fields, &auth_user);

        Ok(Response::new(content::GetGlobalResponse {
            document: Some(proto_doc),
        }))
    }

    /// Update a global's document, running hooks within a transaction.
    pub(super) async fn update_global_impl(
        &self,
        request: Request<content::UpdateGlobalRequest>,
    ) -> Result<Response<content::UpdateGlobalResponse>, Status> {
        let metadata = request.metadata().clone();
        let auth_user = self.extract_auth_user(&metadata);
        let req = request.into_inner();
        let def = self.get_global_def(&req.slug)?;

        // Check update access
        let access_result =
            self.require_access(def.access.update.as_deref(), &auth_user, None, None)?;
        if matches!(access_result, AccessResult::Denied) {
            return Err(Status::permission_denied("Update access denied"));
        }

        // Extract join table data (preserves structured arrays/objects)
        let join_data = req
            .data
            .as_ref()
            .map(prost_struct_to_json_map)
            .unwrap_or_default();

        // Strip field-level update-denied fields
        let mut data = req
            .data
            .map(|s| prost_struct_to_hashmap(&s))
            .unwrap_or_default();
        {
            let user_doc = auth_user.as_ref().map(|au| &au.user_doc);
            let mut conn = self
                .pool
                .get()
                .map_err(|_| Status::internal("Database connection error"))?;
            let tx = conn.transaction()
                .map_err(|e| Status::internal(format!("Transaction error: {}", e)))?;
            let denied =
                self.hook_runner
                    .check_field_write_access(&def.fields, user_doc, "update", &tx);
            let _ = tx.commit();
            for name in &denied {
                data.remove(name);
            }
        }

        let locale_ctx =
            LocaleContext::from_locale_string(req.locale.as_deref(), &self.locale_config);

        let pool = self.pool.clone();
        let runner = self.hook_runner.clone();
        let slug = req.slug.clone();
        let def_fields = def.fields.clone();
        let def_owned = def;
        let user_doc = auth_user.as_ref().map(|au| au.user_doc.clone());
        let (doc, _req_context) = tokio::task::spawn_blocking(move || {
            crate::service::update_global_document(
                &pool,
                &runner,
                &slug,
                &def_owned,
                data,
                &join_data,
                locale_ctx.as_ref(),
                None,
                user_doc.as_ref(),
                false,
            )
        })
        .await
        .map_err(|e| Status::internal(format!("Task error: {}", e)))?
        .map_err(|e| Status::internal(format!("Update global error: {}", e)))?;

        if let Some(c) = &self.populate_cache { c.clear(); }

        {
            let def = self.get_global_def(&req.slug);
            let (hooks, live) = match &def {
                Ok(d) => (d.hooks.clone(), d.live.clone()),
                Err(_) => (Default::default(), None),
            };
            self.hook_runner.publish_event(
                &self.event_bus,
                &hooks,
                live.as_ref(),
                crate::core::event::EventTarget::Global,
                crate::core::event::EventOperation::Update,
                req.slug.clone(),
                doc.id.clone(),
                doc.fields.clone(),
                Self::event_user_from(&auth_user),
            );
        }

        let mut proto_doc = document_to_proto(&doc, &req.slug);
        self.strip_denied_read_fields(&mut proto_doc, &def_fields, &auth_user);

        Ok(Response::new(content::UpdateGlobalResponse {
            document: Some(proto_doc),
        }))
    }

    /// List all registered collections and globals.
    pub(super) async fn list_collections_impl(
        &self,
        _request: Request<content::ListCollectionsRequest>,
    ) -> Result<Response<content::ListCollectionsResponse>, Status> {
        let mut collections: Vec<content::CollectionInfo> = self.registry
            .collections
            .values()
            .map(|def| content::CollectionInfo {
                slug: def.slug.clone(),
                singular_label: def
                    .labels
                    .singular
                    .as_ref()
                    .map(|ls| ls.resolve_default().to_string()),
                plural_label: def
                    .labels
                    .plural
                    .as_ref()
                    .map(|ls| ls.resolve_default().to_string()),
                timestamps: def.timestamps,
                auth: def.is_auth_collection(),
                upload: def.is_upload_collection(),
            })
            .collect();
        collections.sort_by(|a, b| a.slug.cmp(&b.slug));

        let mut globals: Vec<content::GlobalInfo> = self.registry
            .globals
            .values()
            .map(|def| content::GlobalInfo {
                slug: def.slug.clone(),
                singular_label: def
                    .labels
                    .singular
                    .as_ref()
                    .map(|ls| ls.resolve_default().to_string()),
                plural_label: def
                    .labels
                    .plural
                    .as_ref()
                    .map(|ls| ls.resolve_default().to_string()),
            })
            .collect();
        globals.sort_by(|a, b| a.slug.cmp(&b.slug));

        Ok(Response::new(content::ListCollectionsResponse {
            collections,
            globals,
        }))
    }

    /// Describe a collection's schema (fields, timestamps, auth, upload).
    pub(super) async fn describe_collection_impl(
        &self,
        request: Request<content::DescribeCollectionRequest>,
    ) -> Result<Response<content::DescribeCollectionResponse>, Status> {
        let req = request.into_inner();

        if req.is_global {
            let def = self.get_global_def(&req.slug)?;
            Ok(Response::new(content::DescribeCollectionResponse {
                slug: def.slug.clone(),
                singular_label: def
                    .labels
                    .singular
                    .as_ref()
                    .map(|ls| ls.resolve_default().to_string()),
                plural_label: def
                    .labels
                    .plural
                    .as_ref()
                    .map(|ls| ls.resolve_default().to_string()),
                timestamps: false,
                auth: false,
                fields: def.fields.iter().map(field_def_to_proto).collect(),
                upload: false,
                drafts: false,
            }))
        } else {
            let def = self.get_collection_def(&req.slug)?;
            Ok(Response::new(content::DescribeCollectionResponse {
                slug: def.slug.clone(),
                singular_label: def
                    .labels
                    .singular
                    .as_ref()
                    .map(|ls| ls.resolve_default().to_string()),
                plural_label: def
                    .labels
                    .plural
                    .as_ref()
                    .map(|ls| ls.resolve_default().to_string()),
                timestamps: def.timestamps,
                auth: def.is_auth_collection(),
                fields: def.fields.iter().map(field_def_to_proto).collect(),
                upload: def.is_upload_collection(),
                drafts: def.has_drafts(),
            }))
        }
    }

    /// Subscribe to real-time mutation events (server streaming).
    pub(super) async fn subscribe_impl(
        &self,
        request: Request<content::SubscribeRequest>,
    ) -> Result<Response<Pin<Box<dyn Stream<Item = Result<content::MutationEvent, Status>> + Send>>>, Status> {
        let req = request.into_inner();

        let event_bus = self
            .event_bus
            .as_ref()
            .ok_or_else(|| Status::unavailable("Live updates disabled"))?;

        // Authenticate subscriber
        let auth_user = if !req.token.is_empty() {
            let claims = crate::core::auth::validate_token(&req.token, &self.jwt_secret)
                .map_err(|_| Status::unauthenticated("Invalid or expired token"))?;
            let pool = self.pool.clone();
            let registry = self.registry.clone();
            let locale_config = self.locale_config.clone();
            tokio::task::spawn_blocking(move || {
                crate::admin::server::load_auth_user(&pool, &registry, &claims, &locale_config)
            })
            .await
            .map_err(|e| Status::internal(format!("Task error: {}", e)))?
        } else {
            None
        };

        // Build allowed set: snapshot access at subscribe time
        let mut allowed_collections: HashSet<String> = HashSet::new();
        let mut allowed_globals: HashSet<String> = HashSet::new();
        let requested_ops: HashSet<String> = if req.operations.is_empty() {
            ["create", "update", "delete"]
                .iter()
                .map(|s| s.to_string())
                .collect()
        } else {
            req.operations.into_iter().collect()
        };

        {
            let user_doc = auth_user.as_ref().map(|u| &u.user_doc);

            // Check collection read access
            let target_collections: Vec<String> = if req.collections.is_empty() {
                self.registry.collections.keys().cloned().collect()
            } else {
                req.collections
            };

            let mut conn = self
                .pool
                .get()
                .map_err(|e| Status::internal(format!("DB connection: {}", e)))?;
            let tx = conn.transaction()
                .map_err(|e| Status::internal(format!("Transaction error: {}", e)))?;

            for slug in &target_collections {
                if let Some(def) = self.registry.get_collection(slug) {
                    match self.hook_runner.check_access(
                        def.access.read.as_deref(),
                        user_doc,
                        None,
                        None,
                        &tx,
                    ) {
                        Ok(AccessResult::Allowed) | Ok(AccessResult::Constrained(_)) => {
                            allowed_collections.insert(slug.clone());
                        }
                        _ => {}
                    }
                }
            }

            let target_globals: Vec<String> = if req.globals.is_empty() {
                self.registry.globals.keys().cloned().collect()
            } else {
                req.globals
            };

            for slug in &target_globals {
                if let Some(def) = self.registry.get_global(slug) {
                    match self.hook_runner.check_access(
                        def.access.read.as_deref(),
                        user_doc,
                        None,
                        None,
                        &tx,
                    ) {
                        Ok(AccessResult::Allowed) | Ok(AccessResult::Constrained(_)) => {
                            allowed_globals.insert(slug.clone());
                        }
                        _ => {}
                    }
                }
            }
            let _ = tx.commit();
        }

        if allowed_collections.is_empty() && allowed_globals.is_empty() {
            return Err(Status::permission_denied(
                "No accessible collections or globals",
            ));
        }

        let rx = event_bus.subscribe();
        let stream = BroadcastStream::new(rx).filter_map(move |result| {
            match result {
                Ok(event) => {
                    // Filter by target type + collection access
                    let allowed = match event.target {
                        crate::core::event::EventTarget::Collection => {
                            allowed_collections.contains(&event.collection)
                        }
                        crate::core::event::EventTarget::Global => {
                            allowed_globals.contains(&event.collection)
                        }
                    };
                    if !allowed {
                        return None;
                    }

                    // Filter by operation
                    let op_str = match event.operation {
                        crate::core::event::EventOperation::Create => "create",
                        crate::core::event::EventOperation::Update => "update",
                        crate::core::event::EventOperation::Delete => "delete",
                    };
                    if !requested_ops.contains(op_str) {
                        return None;
                    }

                    // Convert data to prost Struct
                    let fields: BTreeMap<String, prost_types::Value> = event
                        .data
                        .iter()
                        .map(|(k, v)| (k.clone(), json_to_prost_value(v)))
                        .collect();

                    let target_str = match event.target {
                        crate::core::event::EventTarget::Collection => "collection",
                        crate::core::event::EventTarget::Global => "global",
                    };

                    Some(Ok(content::MutationEvent {
                        sequence: event.sequence,
                        timestamp: event.timestamp,
                        target: target_str.to_string(),
                        operation: op_str.to_string(),
                        collection: event.collection,
                        document_id: event.document_id,
                        data: Some(prost_types::Struct { fields }),
                    }))
                }
                Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(n)) => {
                    tracing::warn!("Subscribe stream lagged by {} events", n);
                    None
                }
            }
        });

        Ok(Response::new(Box::pin(stream)))
    }

    /// List version history for a document.
    pub(super) async fn list_versions_impl(
        &self,
        request: Request<content::ListVersionsRequest>,
    ) -> Result<Response<content::ListVersionsResponse>, Status> {
        let metadata = request.metadata().clone();
        let auth_user = self.extract_auth_user(&metadata);
        let req = request.into_inner();
        let def = self.get_collection_def(&req.collection)?;

        if !def.has_versions() {
            return Err(Status::failed_precondition(format!(
                "Collection '{}' does not have versioning enabled", req.collection
            )));
        }

        // Check read access
        let access_result =
            self.require_access(def.access.read.as_deref(), &auth_user, Some(&req.id), None)?;
        if matches!(access_result, AccessResult::Denied) {
            return Err(Status::permission_denied("Read access denied"));
        }

        let pool = self.pool.clone();
        let collection = req.collection.clone();
        let id = req.id.clone();
        let limit = req.limit;
        let versions = tokio::task::spawn_blocking(move || {
            let conn = pool.get().context("DB connection")?;
            query::list_versions(&conn, &collection, &id, limit, None)
        })
        .await
        .map_err(|e| Status::internal(format!("Task error: {}", e)))?
        .map_err(|e| Status::internal(format!("List versions error: {}", e)))?;

        let proto_versions: Vec<content::VersionInfo> = versions
            .iter()
            .map(|v| content::VersionInfo {
                id: v.id.clone(),
                version: v.version,
                status: v.status.clone(),
                latest: v.latest,
                created_at: v.created_at.clone().unwrap_or_default(),
            })
            .collect();

        Ok(Response::new(content::ListVersionsResponse {
            versions: proto_versions,
        }))
    }

    /// Restore a document to a previous version.
    pub(super) async fn restore_version_impl(
        &self,
        request: Request<content::RestoreVersionRequest>,
    ) -> Result<Response<content::RestoreVersionResponse>, Status> {
        let metadata = request.metadata().clone();
        let auth_user = self.extract_auth_user(&metadata);
        let req = request.into_inner();
        let def = self.get_collection_def(&req.collection)?;

        if !def.has_versions() {
            return Err(Status::failed_precondition(format!(
                "Collection '{}' does not have versioning enabled", req.collection
            )));
        }

        // Check update access
        let access_result = self.require_access(
            def.access.update.as_deref(),
            &auth_user,
            Some(&req.document_id),
            None,
        )?;
        if matches!(access_result, AccessResult::Denied) {
            return Err(Status::permission_denied("Update access denied"));
        }

        let pool = self.pool.clone();
        let collection = req.collection.clone();
        let document_id = req.document_id.clone();
        let version_id = req.version_id.clone();
        let def_owned = def.clone();
        let locale_config = self.locale_config.clone();
        let doc = tokio::task::spawn_blocking(move || {
            let conn = pool.get().context("DB connection")?;
            let version = query::find_version_by_id(&conn, &collection, &version_id)?
                .ok_or_else(|| anyhow::anyhow!("Version '{}' not found", version_id))?;
            query::restore_version(
                &conn, &collection, &def_owned, &document_id,
                &version.snapshot, &version.status, &locale_config,
            )
        })
        .await
        .map_err(|e| Status::internal(format!("Task error: {}", e)))?
        .map_err(|e| Status::internal(format!("Restore error: {}", e)))?;

        if let Some(c) = &self.populate_cache { c.clear(); }

        let proto_doc = document_to_proto(&doc, &req.collection);

        Ok(Response::new(content::RestoreVersionResponse {
            document: Some(proto_doc),
        }))
    }

    /// List all defined jobs and their configuration.
    pub(super) async fn list_jobs_impl(
        &self,
        request: Request<content::ListJobsRequest>,
    ) -> Result<Response<content::ListJobsResponse>, Status> {
        let metadata = request.metadata().clone();
        let auth_user = self.extract_auth_user(&metadata);
        if auth_user.is_none() {
            return Err(Status::unauthenticated("Authentication required"));
        }

        let jobs: Vec<content::JobDefinitionInfo> = self.registry.jobs.iter().map(|(slug, def)| {
            content::JobDefinitionInfo {
                slug: slug.clone(),
                handler: def.handler.clone(),
                schedule: def.schedule.clone(),
                queue: def.queue.clone(),
                retries: def.retries,
                timeout: def.timeout,
                concurrency: def.concurrency,
                skip_if_running: def.skip_if_running,
                label: def.labels.singular.clone(),
            }
        }).collect();

        Ok(Response::new(content::ListJobsResponse { jobs }))
    }

    /// Trigger a job by slug, queuing it for execution.
    pub(super) async fn trigger_job_impl(
        &self,
        request: Request<content::TriggerJobRequest>,
    ) -> Result<Response<content::TriggerJobResponse>, Status> {
        let metadata = request.metadata().clone();
        let auth_user = self.extract_auth_user(&metadata);
        if auth_user.is_none() {
            return Err(Status::unauthenticated("Authentication required"));
        }
        let req = request.into_inner();

        // Look up job definition
        let job_def = self.registry.get_job(&req.slug).cloned()
            .ok_or_else(|| Status::not_found(format!("Job '{}' not found", req.slug)))?;

        // Check access if defined
        if let Some(ref access_ref) = job_def.access {
            let result = self.require_access(Some(access_ref), &auth_user, None, None)?;
            if matches!(result, AccessResult::Denied) {
                return Err(Status::permission_denied("Trigger access denied"));
            }
        }

        let data_json = req.data_json.unwrap_or_else(|| "{}".to_string());
        let conn = self.pool.get()
            .map_err(|_| Status::internal("Database connection error"))?;

        let job_run = crate::db::query::jobs::insert_job(
            &conn,
            &req.slug,
            &data_json,
            "grpc",
            job_def.retries + 1,
            &job_def.queue,
        ).map_err(|e| Status::internal(format!("Failed to queue job: {}", e)))?;

        Ok(Response::new(content::TriggerJobResponse {
            job_id: job_run.id,
        }))
    }

    /// Get details of a specific job run.
    pub(super) async fn get_job_run_impl(
        &self,
        request: Request<content::GetJobRunRequest>,
    ) -> Result<Response<content::GetJobRunResponse>, Status> {
        let metadata = request.metadata().clone();
        let auth_user = self.extract_auth_user(&metadata);
        if auth_user.is_none() {
            return Err(Status::unauthenticated("Authentication required"));
        }
        let req = request.into_inner();

        let conn = self.pool.get()
            .map_err(|_| Status::internal("Database connection error"))?;

        let run = crate::db::query::jobs::get_job_run(&conn, &req.id)
            .map_err(|e| Status::internal(format!("Query error: {}", e)))?
            .ok_or_else(|| Status::not_found(format!("Job run '{}' not found", req.id)))?;

        Ok(Response::new(job_run_to_proto(&run)))
    }

    /// List job runs with optional filters.
    pub(super) async fn list_job_runs_impl(
        &self,
        request: Request<content::ListJobRunsRequest>,
    ) -> Result<Response<content::ListJobRunsResponse>, Status> {
        let metadata = request.metadata().clone();
        let auth_user = self.extract_auth_user(&metadata);
        if auth_user.is_none() {
            return Err(Status::unauthenticated("Authentication required"));
        }
        let req = request.into_inner();

        let conn = self.pool.get()
            .map_err(|_| Status::internal("Database connection error"))?;

        let limit = req.limit.unwrap_or(50);
        let offset = req.offset.unwrap_or(0);

        let runs = crate::db::query::jobs::list_job_runs(
            &conn,
            req.slug.as_deref(),
            req.status.as_deref(),
            limit,
            offset,
        ).map_err(|e| Status::internal(format!("Query error: {}", e)))?;

        let runs: Vec<content::GetJobRunResponse> = runs.iter()
            .map(job_run_to_proto)
            .collect();

        Ok(Response::new(content::ListJobRunsResponse { runs }))
    }
}

/// Convert a JobRun to gRPC response.
#[cfg(not(tarpaulin_include))]
fn job_run_to_proto(run: &crate::core::job::JobRun) -> content::GetJobRunResponse {
    content::GetJobRunResponse {
        id: run.id.clone(),
        slug: run.slug.clone(),
        status: run.status.as_str().to_string(),
        data_json: run.data.clone(),
        result_json: run.result.clone(),
        error: run.error.clone(),
        attempt: run.attempt,
        max_attempts: run.max_attempts,
        scheduled_by: run.scheduled_by.clone(),
        created_at: run.created_at.clone(),
        started_at: run.started_at.clone(),
        completed_at: run.completed_at.clone(),
    }
}
