//! Schema/metadata RPC handlers: GetGlobal, UpdateGlobal, ListCollections,
//! DescribeCollection, Subscribe, ListVersions, RestoreVersion, ListJobs,
//! TriggerJob, GetJobRun, ListJobRuns.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
};

use tokio::task;
use tokio_stream::{Stream, StreamExt, wrappers::BroadcastStream};
use tonic::{Request, Response, Status};
use tracing::{error, warn};

use crate::{
    api::{
        content,
        service::{
            ContentService,
            collection::helpers::strip_denied_proto_fields,
            convert::{
                document_to_proto, field_def_to_proto, json_to_prost_value,
                prost_struct_to_hashmap, prost_struct_to_json_map,
            },
        },
    },
    core::{
        event::{EventOperation, EventTarget},
        job::JobRun,
    },
    db::{
        AccessResult, LocaleContext,
        query::{self, jobs},
    },
    hooks::lifecycle::{AfterReadCtx, PublishEventInput},
    service::{self, WriteInput},
};

/// Atomically try to acquire a Subscribe connection slot.
///
/// Returns `true` if a slot was acquired (counter incremented), `false` if the
/// limit has been reached. When `max == 0`, no limit is enforced (always succeeds).
/// Uses `compare_exchange_weak` in a loop to avoid the TOCTOU race inherent in
/// `fetch_add` + check + `fetch_sub`.
fn try_acquire_subscribe_slot(counter: &AtomicUsize, max: usize) -> bool {
    loop {
        let current = counter.load(Ordering::Relaxed);

        if max > 0 && current >= max {
            return false;
        }

        if counter
            .compare_exchange_weak(current, current + 1, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            return true;
        }
    }
}

/// RAII guard that decrements the Subscribe connection counter on drop.
struct SubscribeConnectionGuard {
    counter: Arc<AtomicUsize>,
}

impl Drop for SubscribeConnectionGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Stream wrapper that holds a connection guard, releasing it when the stream ends.
struct GuardedStream<S> {
    inner: Pin<Box<S>>,
    _guard: SubscribeConnectionGuard,
}

impl<S: Stream + Unpin> Stream for GuardedStream<S> {
    type Item = S::Item;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.inner.as_mut().poll_next(cx)
    }
}

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
        let token = Self::extract_token(&metadata);
        let req = request.into_inner();
        let def = self.get_global_def(&req.slug)?;

        let locale_ctx =
            LocaleContext::from_locale_string(req.locale.as_deref(), &self.locale_config);

        let pool = self.pool.clone();
        let runner = self.hook_runner.clone();
        let token_provider = self.token_provider.clone();
        let registry = self.registry.clone();
        let hooks = def.hooks.clone();
        let def_fields = def.fields.clone();
        let fields = def_fields.clone();
        let slug = req.slug.clone();
        let proto_doc = task::spawn_blocking(move || -> Result<_, Status> {
            let mut conn = pool.get().map_err(|e| {
                error!("GetGlobal pool error: {}", e);

                Status::internal("Internal error")
            })?;

            let auth_user =
                ContentService::resolve_auth_user(token, &*token_provider, &registry, &conn)?;
            let access_result = ContentService::check_access_blocking(
                def.access.read.as_deref(),
                &auth_user,
                None,
                None,
                &runner,
                &mut conn,
            )?;

            if matches!(access_result, AccessResult::Denied) {
                return Err(Status::permission_denied("Read access denied"));
            }

            runner
                .fire_before_read(&hooks, &slug, "get_global", HashMap::new())
                .map_err(|e| {
                    error!("GetGlobal hook error: {}", e);

                    Status::internal("Internal error")
                })?;
            let doc = query::get_global(&conn, &slug, &def, locale_ctx.as_ref()).map_err(|e| {
                error!("GetGlobal query error: {}", e);

                Status::internal("Internal error")
            })?;
            let ar_ctx = AfterReadCtx {
                hooks: &hooks,
                fields: &fields,
                collection: &slug,
                operation: "get_global",
                user: auth_user.as_ref().map(|au| &au.user_doc),
                ui_locale: None,
            };
            let doc = runner.apply_after_read(&ar_ctx, doc);

            let mut proto_doc = document_to_proto(&doc, &slug);
            let user_doc = auth_user.as_ref().map(|au| &au.user_doc);
            let tx = conn.transaction().map_err(|e| {
                error!("Field access check tx error: {}", e);

                Status::internal("Internal error")
            })?;
            let denied = runner.check_field_read_access(&def_fields, user_doc, &tx);

            if let Err(e) = tx.commit() {
                warn!("tx commit failed: {e}");
            }

            strip_denied_proto_fields(&mut proto_doc, &denied);

            Ok(proto_doc)
        })
        .await
        .map_err(|e| {
            error!("GetGlobal task error: {}", e);

            Status::internal("Internal error")
        })??;

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
        let token = Self::extract_token(&metadata);
        let req = request.into_inner();
        let def = self.get_global_def(&req.slug)?;

        // Extract join table data (preserves structured arrays/objects)
        let mut join_data = req
            .data
            .as_ref()
            .map(prost_struct_to_json_map)
            .unwrap_or_default();

        let mut data = req
            .data
            .map(|s| prost_struct_to_hashmap(&s))
            .unwrap_or_default();

        let locale_ctx =
            LocaleContext::from_locale_string(req.locale.as_deref(), &self.locale_config);

        let pool = self.pool.clone();
        let runner = self.hook_runner.clone();
        let token_provider = self.token_provider.clone();
        let registry = self.registry.clone();
        let slug = req.slug.clone();
        let def_fields = def.fields.clone();
        let def_owned = def;
        let (proto_doc, auth_user) = task::spawn_blocking(move || -> Result<_, Status> {
            let mut conn = pool.get().map_err(|e| {
                error!("UpdateGlobal pool error: {}", e);

                Status::internal("Internal error")
            })?;

            let auth_user =
                ContentService::resolve_auth_user(token, &*token_provider, &registry, &conn)?;
            let access_result = ContentService::check_access_blocking(
                def_owned.access.update.as_deref(),
                &auth_user,
                None,
                None,
                &runner,
                &mut conn,
            )?;

            if matches!(access_result, AccessResult::Denied) {
                return Err(Status::permission_denied("Update access denied"));
            }

            // Strip field-level update-denied fields
            {
                let user_doc = auth_user.as_ref().map(|au| &au.user_doc);
                let tx = conn.transaction().map_err(|e| {
                    error!("UpdateGlobal field access tx error: {}", e);

                    Status::internal("Internal error")
                })?;
                let denied =
                    runner.check_field_write_access(&def_owned.fields, user_doc, "update", &tx);
                if let Err(e) = tx.commit() {
                    warn!("tx commit failed: {e}");
                }
                for name in &denied {
                    data.remove(name);
                    join_data.remove(name);
                }
            }

            let user_doc = auth_user.as_ref().map(|au| au.user_doc.clone());
            let ui_locale = auth_user.as_ref().map(|au| au.ui_locale.clone());

            drop(conn);

            let (doc, _req_context) = service::update_global_document(
                &pool,
                &runner,
                &slug,
                &def_owned,
                WriteInput::builder(data, &join_data)
                    .locale_ctx(locale_ctx.as_ref())
                    .ui_locale(ui_locale)
                    .build(),
                user_doc.as_ref(),
            )
            .map_err(|e| {
                error!("UpdateGlobal error: {}", e);

                Status::internal("Internal error")
            })?;

            // Proto conversion + field stripping
            let mut proto_doc = document_to_proto(&doc, &slug);
            let user_doc_ref = auth_user.as_ref().map(|au| &au.user_doc);
            let mut conn2 = pool.get().map_err(|e| {
                error!("UpdateGlobal field access pool error: {}", e);

                Status::internal("Internal error")
            })?;
            let tx = conn2.transaction().map_err(|e| {
                error!("Field read access tx error: {}", e);

                Status::internal("Internal error")
            })?;
            let denied = runner.check_field_read_access(&def_fields, user_doc_ref, &tx);

            if let Err(e) = tx.commit() {
                warn!("tx commit failed: {e}");
            }

            strip_denied_proto_fields(&mut proto_doc, &denied);

            Ok((proto_doc, auth_user))
        })
        .await
        .map_err(|e| {
            error!("UpdateGlobal task error: {}", e);

            Status::internal("Internal error")
        })??;

        if let Err(e) = self.cache.clear() {
            warn!("Cache clear failed: {:#}", e);
        }

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
                PublishEventInput::builder(EventTarget::Global, EventOperation::Update)
                    .collection(req.slug.clone())
                    .document_id(proto_doc.id.clone())
                    .edited_by(Self::event_user_from(&auth_user))
                    .build(),
            );
        }

        Ok(Response::new(content::UpdateGlobalResponse {
            document: Some(proto_doc),
        }))
    }

    /// List all registered collections and globals.
    pub(super) async fn list_collections_impl(
        &self,
        _request: Request<content::ListCollectionsRequest>,
    ) -> Result<Response<content::ListCollectionsResponse>, Status> {
        let mut collections: Vec<content::CollectionInfo> = self
            .registry
            .collections
            .values()
            .map(|def| content::CollectionInfo {
                slug: def.slug.to_string(),
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

        let mut globals: Vec<content::GlobalInfo> = self
            .registry
            .globals
            .values()
            .map(|def| content::GlobalInfo {
                slug: def.slug.to_string(),
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
            let singular_label = def
                .labels
                .singular
                .as_ref()
                .map(|ls| ls.resolve_default().to_string());
            let plural_label = def
                .labels
                .plural
                .as_ref()
                .map(|ls| ls.resolve_default().to_string());
            let fields = def.fields.iter().map(field_def_to_proto).collect();

            Ok(Response::new(content::DescribeCollectionResponse {
                slug: def.slug.to_string(),
                singular_label,
                plural_label,
                timestamps: false,
                auth: false,
                fields,
                upload: false,
                drafts: false,
            }))
        } else {
            let def = self.get_collection_def(&req.slug)?;
            let singular_label = def
                .labels
                .singular
                .as_ref()
                .map(|ls| ls.resolve_default().to_string());
            let plural_label = def
                .labels
                .plural
                .as_ref()
                .map(|ls| ls.resolve_default().to_string());
            let fields = def.fields.iter().map(field_def_to_proto).collect();

            Ok(Response::new(content::DescribeCollectionResponse {
                slug: def.slug.to_string(),
                singular_label,
                plural_label,
                timestamps: def.timestamps,
                auth: def.is_auth_collection(),
                fields,
                upload: def.is_upload_collection(),
                drafts: def.has_drafts(),
            }))
        }
    }

    /// Subscribe to real-time mutation events (server streaming).
    pub(super) async fn subscribe_impl(
        &self,
        request: Request<content::SubscribeRequest>,
    ) -> Result<
        Response<Pin<Box<dyn Stream<Item = Result<content::MutationEvent, Status>> + Send>>>,
        Status,
    > {
        // Enforce Subscribe connection limit (race-free via compare_exchange)
        let max = self.max_subscribe_connections;

        if !try_acquire_subscribe_slot(&self.subscribe_connections, max) {
            warn!(
                "Subscribe connection limit reached ({}/{}), rejecting",
                max, max
            );

            return Err(Status::resource_exhausted("Too many Subscribe streams"));
        }

        let subscribe_guard = SubscribeConnectionGuard {
            counter: self.subscribe_connections.clone(),
        };

        let metadata = request.metadata().clone();
        let req = request.into_inner();

        let event_bus = self
            .event_bus
            .as_ref()
            .ok_or_else(|| Status::unavailable("Live updates disabled"))?;

        let token = Self::extract_token(&metadata);

        let requested_ops: HashSet<String> = if req.operations.is_empty() {
            ["create", "update", "delete"]
                .iter()
                .map(|s| s.to_string())
                .collect()
        } else {
            req.operations.into_iter().collect()
        };

        let pool = self.pool.clone();
        let token_provider = self.token_provider.clone();
        let registry = self.registry.clone();
        let hook_runner = self.hook_runner.clone();
        let collections_req = req.collections;
        let globals_req = req.globals;
        let (allowed_collections, allowed_globals) = task::spawn_blocking(move || {
            let mut conn = pool.get().map_err(|e| {
                error!("Subscribe pool error: {}", e);

                Status::internal("Internal error")
            })?;

            let auth_user =
                ContentService::resolve_auth_user(token, &*token_provider, &registry, &conn)?;
            let user_doc = auth_user.as_ref().map(|u| &u.user_doc);

            let tx = conn.transaction().map_err(|e| {
                error!("Subscribe tx error: {}", e);

                Status::internal("Internal error")
            })?;

            // Check collection read access
            let target_collections: Vec<String> = if collections_req.is_empty() {
                registry.collections.keys().map(|s| s.to_string()).collect()
            } else {
                collections_req
            };

            let mut allowed_collections: HashSet<String> = HashSet::new();
            for slug in &target_collections {
                if let Some(def) = registry.get_collection(slug) {
                    match hook_runner.check_access(
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

            let target_globals: Vec<String> = if globals_req.is_empty() {
                registry.globals.keys().map(|s| s.to_string()).collect()
            } else {
                globals_req
            };

            let mut allowed_globals: HashSet<String> = HashSet::new();

            for slug in &target_globals {
                if let Some(def) = registry.get_global(slug) {
                    match hook_runner.check_access(
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

            if let Err(e) = tx.commit() {
                warn!("tx commit failed: {e}");
            }

            Ok::<_, Status>((allowed_collections, allowed_globals))
        })
        .await
        .map_err(|e| {
            error!("Subscribe task error: {}", e);

            Status::internal("Internal error")
        })??;

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
                        EventTarget::Collection => {
                            allowed_collections.contains(event.collection.as_ref() as &str)
                        }
                        EventTarget::Global => {
                            allowed_globals.contains(event.collection.as_ref() as &str)
                        }
                    };

                    if !allowed {
                        return None;
                    }

                    // Filter by operation
                    let op_str = match event.operation {
                        EventOperation::Create => "create",
                        EventOperation::Update => "update",
                        EventOperation::Delete => "delete",
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
                        EventTarget::Collection => "collection",
                        EventTarget::Global => "global",
                    };

                    Some(Ok(content::MutationEvent {
                        sequence: event.sequence,
                        timestamp: event.timestamp,
                        target: target_str.to_string(),
                        operation: op_str.to_string(),
                        collection: event.collection.to_string(),
                        document_id: event.document_id.to_string(),
                        data: Some(prost_types::Struct { fields }),
                    }))
                }
                Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(n)) => {
                    tracing::warn!("Subscribe stream lagged by {} events", n);
                    None
                }
            }
        });

        // Attach the connection guard to the stream so it decrements on drop
        let guarded = GuardedStream {
            inner: Box::pin(stream),
            _guard: subscribe_guard,
        };

        Ok(Response::new(Box::pin(guarded)
            as Pin<
                Box<dyn Stream<Item = Result<content::MutationEvent, Status>> + Send>,
            >))
    }

    /// List version history for a document.
    pub(super) async fn list_versions_impl(
        &self,
        request: Request<content::ListVersionsRequest>,
    ) -> Result<Response<content::ListVersionsResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let req = request.into_inner();
        let def = self.get_collection_def(&req.collection)?;

        if !def.has_versions() {
            return Err(Status::failed_precondition(format!(
                "Collection '{}' does not have versioning enabled",
                req.collection
            )));
        }

        let pool = self.pool.clone();
        let runner = self.hook_runner.clone();
        let token_provider = self.token_provider.clone();
        let registry = self.registry.clone();
        let collection = req.collection.clone();
        let id = req.id.clone();
        let limit = req.limit;
        let access_read = def.access.read.clone();
        let versions = task::spawn_blocking(move || -> Result<_, Status> {
            let mut conn = pool.get().map_err(|e| {
                error!("ListVersions pool error: {}", e);

                Status::internal("Internal error")
            })?;

            let auth_user =
                ContentService::resolve_auth_user(token, &*token_provider, &registry, &conn)?;
            let access_result = ContentService::check_access_blocking(
                access_read.as_deref(),
                &auth_user,
                Some(&id),
                None,
                &runner,
                &mut conn,
            )?;

            if matches!(access_result, AccessResult::Denied) {
                return Err(Status::permission_denied("Read access denied"));
            }

            query::list_versions(&conn, &collection, &id, limit, None).map_err(|e| {
                error!("ListVersions error: {}", e);

                Status::internal("Internal error")
            })
        })
        .await
        .map_err(|e| {
            error!("ListVersions task error: {}", e);

            Status::internal("Internal error")
        })??;

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
        let token = Self::extract_token(&metadata);
        let req = request.into_inner();
        let def = self.get_collection_def(&req.collection)?;

        if !def.has_versions() {
            return Err(Status::failed_precondition(format!(
                "Collection '{}' does not have versioning enabled",
                req.collection
            )));
        }

        let pool = self.pool.clone();
        let runner = self.hook_runner.clone();
        let token_provider = self.token_provider.clone();
        let registry = self.registry.clone();
        let collection = req.collection.clone();
        let document_id = req.document_id.clone();
        let version_id = req.version_id.clone();
        let access_update = def.access.update.clone();
        let def_owned = def.clone();
        let locale_config = self.locale_config.clone();
        let doc = task::spawn_blocking(move || -> Result<_, Status> {
            let mut conn = pool.get().map_err(|e| {
                error!("RestoreVersion pool error: {}", e);

                Status::internal("Internal error")
            })?;

            let auth_user =
                ContentService::resolve_auth_user(token, &*token_provider, &registry, &conn)?;
            let access_result = ContentService::check_access_blocking(
                access_update.as_deref(),
                &auth_user,
                Some(&document_id),
                None,
                &runner,
                &mut conn,
            )?;

            if matches!(access_result, AccessResult::Denied) {
                return Err(Status::permission_denied("Update access denied"));
            }

            let tx = conn.transaction_immediate().map_err(|e| {
                error!("RestoreVersion tx error: {}", e);

                Status::internal("Internal error")
            })?;

            let version = query::find_version_by_id(&tx, &collection, &version_id)
                .map_err(|e| {
                    error!("RestoreVersion error: {}", e);

                    Status::internal("Internal error")
                })?
                .ok_or_else(|| Status::not_found(format!("Version '{}' not found", version_id)))?;

            let doc = query::restore_version(
                &tx,
                &collection,
                &def_owned,
                &document_id,
                &version.snapshot,
                &version.status,
                &locale_config,
            )
            .map_err(|e| {
                error!("RestoreVersion error: {}", e);

                Status::internal("Internal error")
            })?;

            tx.commit().map_err(|e| {
                error!("RestoreVersion commit error: {}", e);

                Status::internal("Internal error")
            })?;

            Ok(doc)
        })
        .await
        .map_err(|e| {
            error!("RestoreVersion task error: {}", e);

            Status::internal("Internal error")
        })??;

        if let Err(e) = self.cache.clear() {
            warn!("Cache clear failed: {:#}", e);
        }

        let mut proto_doc = document_to_proto(&doc, &req.collection);

        // Strip field-level read-denied fields (parity with other endpoints)
        {
            let pool = self.pool.clone();
            let runner = self.hook_runner.clone();
            let token_provider = self.token_provider.clone();
            let registry = self.registry.clone();
            let def_fields = def.fields.clone();
            let metadata2 = metadata.clone();
            let denied = task::spawn_blocking(move || -> Result<Vec<String>, Status> {
                let mut conn = pool.get().map_err(|e| {
                    error!("RestoreVersion field access pool error: {}", e);

                    Status::internal("Internal error")
                })?;
                let token = Self::extract_token(&metadata2);
                let auth_user =
                    ContentService::resolve_auth_user(token, &*token_provider, &registry, &conn)?;
                let user_doc = auth_user.as_ref().map(|au| &au.user_doc);
                let tx = conn.transaction().map_err(|e| {
                    error!("Field access tx error: {}", e);

                    Status::internal("Internal error")
                })?;
                let denied = runner.check_field_read_access(&def_fields, user_doc, &tx);

                if let Err(e) = tx.commit() {
                    warn!("tx commit failed: {e}");
                }

                Ok(denied)
            })
            .await
            .map_err(|e| {
                error!("RestoreVersion field access task error: {}", e);

                Status::internal("Internal error")
            })??;

            strip_denied_proto_fields(&mut proto_doc, &denied);
        }

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
        .map_err(|e| {
            error!("ListJobs task error: {}", e);

            Status::internal("Internal error")
        })??;

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

    /// Trigger a job by slug, queuing it for execution.
    pub(super) async fn trigger_job_impl(
        &self,
        request: Request<content::TriggerJobRequest>,
    ) -> Result<Response<content::TriggerJobResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let req = request.into_inner();

        let pool = self.pool.clone();
        let hook_runner = self.hook_runner.clone();
        let token_provider = self.token_provider.clone();
        let registry = self.registry.clone();
        let data_json = req.data_json.unwrap_or_else(|| "{}".to_string());
        let slug = req.slug.clone();
        let job_id = task::spawn_blocking(move || -> Result<String, Status> {
            let mut conn = pool.get().map_err(|e| {
                error!("TriggerJob pool error: {}", e);

                Status::internal("Internal error")
            })?;

            // Auth check first — before job lookup
            let auth_user =
                ContentService::resolve_auth_user(token, &*token_provider, &registry, &conn)?;
            if auth_user.is_none() {
                return Err(Status::unauthenticated("Authentication required"));
            }

            // Look up job definition
            let job_def = registry
                .get_job(&slug)
                .cloned()
                .ok_or_else(|| Status::not_found(format!("Job '{}' not found", slug)))?;

            // Check access if defined
            if job_def.access.is_some() {
                let result = ContentService::check_access_blocking(
                    job_def.access.as_deref(),
                    &auth_user,
                    None,
                    None,
                    &hook_runner,
                    &mut conn,
                )?;

                if matches!(result, AccessResult::Denied) {
                    return Err(Status::permission_denied("Trigger access denied"));
                }
            }

            let job_run = jobs::insert_job(
                &conn,
                &slug,
                &data_json,
                "grpc",
                job_def.retries + 1,
                &job_def.queue,
            )
            .map_err(|e| {
                error!("Failed to queue job: {}", e);

                Status::internal("Internal error")
            })?;

            Ok(job_run.id)
        })
        .await
        .map_err(|e| {
            error!("TriggerJob task error: {}", e);

            Status::internal("Internal error")
        })??;

        Ok(Response::new(content::TriggerJobResponse { job_id }))
    }

    /// Get details of a specific job run.
    pub(super) async fn get_job_run_impl(
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

            jobs::get_job_run(&conn, &id)
                .map_err(|e| {
                    error!("GetJobRun query error: {}", e);

                    Status::internal("Internal error")
                })?
                .ok_or_else(|| Status::not_found(format!("Job run '{}' not found", id)))
        })
        .await
        .map_err(|e| {
            error!("GetJobRun task error: {}", e);

            Status::internal("Internal error")
        })??;

        Ok(Response::new(job_run_to_proto(&run)))
    }

    /// List job runs with optional filters.
    pub(super) async fn list_job_runs_impl(
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
        .map_err(|e| {
            error!("ListJobRuns task error: {}", e);

            Status::internal("Internal error")
        })??;

        let runs: Vec<content::GetJobRunResponse> = runs.iter().map(job_run_to_proto).collect();

        Ok(Response::new(content::ListJobRunsResponse { runs }))
    }
}

/// Convert a JobRun to gRPC response.
#[cfg(not(tarpaulin_include))]
fn job_run_to_proto(run: &JobRun) -> content::GetJobRunResponse {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscribe_slot_acquire_within_limit() {
        let counter = AtomicUsize::new(0);
        assert!(try_acquire_subscribe_slot(&counter, 10));
        assert_eq!(counter.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn subscribe_slot_acquire_at_limit() {
        let counter = AtomicUsize::new(5);
        assert!(!try_acquire_subscribe_slot(&counter, 5));
        assert_eq!(counter.load(Ordering::Relaxed), 5);
    }

    #[test]
    fn subscribe_slot_acquire_no_limit() {
        let counter = AtomicUsize::new(1000);
        assert!(try_acquire_subscribe_slot(&counter, 0));
        assert_eq!(counter.load(Ordering::Relaxed), 1001);
    }

    #[test]
    fn subscribe_slot_fills_to_limit() {
        let counter = AtomicUsize::new(0);
        for _ in 0..3 {
            assert!(try_acquire_subscribe_slot(&counter, 3));
        }
        assert!(!try_acquire_subscribe_slot(&counter, 3));
        assert_eq!(counter.load(Ordering::Relaxed), 3);
    }
}
