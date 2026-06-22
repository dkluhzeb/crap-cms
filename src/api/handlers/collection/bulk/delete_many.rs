//! Bulk `DeleteMany` RPC handler.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::hooks::AccessCheckInput;
use crate::{
    api::{content, handlers::ContentService},
    config::LocaleConfig,
    core::{
        CollectionDefinition, Registry, SharedCache, SharedEventTransport,
        SharedInvalidationTransport, SharedStorage, SharedTokenProvider, upload,
    },
    db::{AccessResult, DbPool},
    hooks::HookRunner,
    service::{DeleteManyOptions, DeleteManyResult, ServiceContext, ServiceError, delete_many},
};

use super::helpers::build_bulk_filters;

/// Owned bundle for the `DeleteMany` spawn-blocking body.
struct DeleteManyBlockingInput {
    pool: DbPool,
    hook_runner: HookRunner,
    headers: HashMap<String, String>,
    token_provider: SharedTokenProvider,
    registry: Arc<Registry>,
    db_kind: String,
    collection: String,
    where_json: Option<String>,
    storage: SharedStorage,
    locale_cfg: LocaleConfig,
    def: CollectionDefinition,
    invalidation_transport: SharedInvalidationTransport,
    event_transport: Option<SharedEventTransport>,
    cache: Option<SharedCache>,
    token: Option<String>,
    run_hooks: bool,
    bulk_max_documents: i64,
    events: bool,
}

fn delete_many_blocking(input: DeleteManyBlockingInput) -> Result<DeleteManyResult, Status> {
    let mut conn = input
        .pool
        .get()
        .map_err(|e| Status::from(ServiceError::classify(e, &input.db_kind)))?;

    let auth_user = ContentService::resolve_auth_user(
        input.token.as_deref(),
        &input.headers,
        &*input.token_provider,
        &input.hook_runner,
        &input.registry,
        &conn,
    )?;

    let user_doc = auth_user.as_ref().map(|au| &au.user_doc);
    let read_access = ContentService::check_access_blocking(
        &AccessCheckInput {
            document: None,
            access: input.def.access.read.as_ref(),
            user: user_doc,
            id: None,
            data: None,
            locale: None,
            operation: "find",
            collection: &input.def.slug,
            ui_locale: None,
        },
        &input.hook_runner,
        &mut conn,
    )?;

    if matches!(read_access, AccessResult::Denied) {
        return Err(Status::permission_denied("Read access denied"));
    }

    drop(conn);

    let filters = build_bulk_filters(
        &input.collection,
        &input.def,
        &read_access,
        input.where_json.as_deref(),
        true,
    )?;

    let user_doc = auth_user.as_ref().map(|au| &au.user_doc);

    let ctx = ServiceContext::collection(&input.collection, &input.def)
        .pool(&input.pool)
        .runner(&input.hook_runner)
        .user(user_doc)
        .invalidation_transport(Some(input.invalidation_transport))
        .event_transport(input.event_transport)
        .emit_events(input.events)
        .cache(input.cache)
        .build();

    let delete_opts = DeleteManyOptions {
        run_hooks: input.run_hooks,
        max_documents: input.bulk_max_documents,
        ..Default::default()
    };

    let result = delete_many(&ctx, &filters, &input.locale_cfg, &delete_opts)
        .map_err(|e| Status::from(e.reclassify(&input.db_kind)))?;

    for fields in &result.upload_fields_to_clean {
        upload::delete_upload_files(&*input.storage, fields);
    }

    Ok(result)
}

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Bulk delete matching documents. Runs per-document lifecycle hooks by default.
    pub(in crate::api::handlers) async fn delete_many_impl(
        &self,
        request: Request<content::DeleteManyRequest>,
    ) -> Result<Response<content::DeleteManyResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let headers = self.metadata_headers(&metadata);
        let req = request.into_inner();
        let mut def = self.get_collection_def(&req.collection)?;
        let run_hooks = req.hooks.unwrap_or(true);

        if req.force_hard_delete && def.soft_delete {
            def.soft_delete = false;
        }

        let input = DeleteManyBlockingInput {
            pool: self.pool.clone(),
            hook_runner: self.hook_runner.clone(),
            token_provider: self.token_provider.clone(),
            registry: Arc::clone(&self.registry),
            db_kind: self.db_kind.clone(),
            collection: req.collection.clone(),
            where_json: req.r#where.clone(),
            storage: self.storage.clone(),
            locale_cfg: self.locale_config.clone(),
            def,
            invalidation_transport: self.invalidation_transport.clone(),
            event_transport: self.event_transport.clone(),
            cache: Some(self.cache.clone()),
            token,
            headers,
            run_hooks,
            bulk_max_documents: self.server_config.bulk_max_documents,
            events: req.events.unwrap_or(false),
        };

        let result = task::spawn_blocking(move || delete_many_blocking(input))
            .await
            .inspect_err(|e| error!("Task error: {}", e))
            .map_err(|_| Status::internal("Internal error"))??;

        Ok(Response::new(content::DeleteManyResponse {
            deleted: result.hard_deleted,
            soft_deleted: result.soft_deleted,
            skipped: result.skipped,
        }))
    }
}
