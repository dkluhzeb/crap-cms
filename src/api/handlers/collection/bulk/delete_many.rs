//! Bulk `DeleteMany` RPC handler.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::hooks::AccessCheckInput;
use crate::{
    api::{content, handlers::ContentService},
    core::{CollectionDefinition, upload},
    db::AccessResult,
    service::{
        AppInfra, DeleteManyOptions, DeleteManyResult, ServiceContext, ServiceError, delete_many,
    },
};

use super::helpers::build_bulk_filters;

/// Owned bundle for the `DeleteMany` spawn-blocking body. Process-stable
/// dependencies come from the shared [`AppInfra`]; the rest is per-call.
struct DeleteManyBlockingInput {
    infra: Arc<AppInfra>,
    headers: HashMap<String, String>,
    db_kind: String,
    collection: String,
    where_json: Option<String>,
    def: CollectionDefinition,
    token: Option<String>,
    run_hooks: bool,
    bulk_max_documents: i64,
    events: bool,
}

fn delete_many_blocking(input: &DeleteManyBlockingInput) -> Result<DeleteManyResult, Status> {
    let infra = &input.infra;

    let mut conn = infra
        .pool
        .get()
        .map_err(|e| Status::from(ServiceError::classify(e, &input.db_kind)))?;

    let auth_user = ContentService::resolve_auth_user(
        input.token.as_deref(),
        &input.headers,
        &*infra.token_provider,
        &infra.hook_runner,
        &infra.registry,
        &conn,
    )?;

    let user_doc = auth_user.as_ref().map(|au| &au.user_doc);
    let read_access = ContentService::check_access_blocking(
        &AccessCheckInput::builder("find", &input.def.slug)
            .access(input.def.access.read.as_ref())
            .user(user_doc)
            .build(),
        &infra.hook_runner,
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
        .infra(infra)
        .user(user_doc)
        .emit_events(input.events)
        .build();

    let delete_opts = DeleteManyOptions {
        run_hooks: input.run_hooks,
        max_documents: input.bulk_max_documents,
        ..Default::default()
    };

    let result = delete_many(&ctx, &filters, &infra.locale_config, &delete_opts)
        .map_err(|e| Status::from(e.reclassify(&input.db_kind)))?;

    for fields in &result.upload_fields_to_clean {
        upload::delete_upload_files(&*infra.storage, fields);
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
            def.make_hard_delete();
        }

        let input = DeleteManyBlockingInput {
            infra: Arc::clone(&self.infra),
            db_kind: self.db_kind.clone(),
            collection: req.collection.clone(),
            where_json: req.r#where.clone(),
            def,
            token,
            headers,
            run_hooks,
            bulk_max_documents: self.server_config.bulk_max_documents,
            events: req.events.unwrap_or(false),
        };

        let result = task::spawn_blocking(move || delete_many_blocking(&input))
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
