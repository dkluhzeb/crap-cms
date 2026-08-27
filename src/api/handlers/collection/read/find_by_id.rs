//! `FindByID` handler — fetch a single document by ID.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{
        content,
        handlers::{ContentService, proto::document_to_proto},
    },
    core::CollectionDefinition,
    db::{LocaleContext, query},
    service::{
        AppInfra, FindByIdInput, RunnerReadHooks, ServiceContext, ServiceError, find_document_by_id,
    },
};

/// Owned bundle for the `FindById` spawn-blocking body. Process-stable
/// dependencies come from the shared [`AppInfra`]; the rest is per-call.
struct FindByIdBlockingInput {
    infra: Arc<AppInfra>,
    headers: HashMap<String, String>,
    db_kind: String,
    collection: String,
    id: String,
    def: CollectionDefinition,
    token: Option<String>,
    depth: i32,
    select: Option<Vec<String>>,
    locale_ctx: Option<LocaleContext>,
    use_draft_version: bool,
    include_deleted: bool,
}

fn find_by_id_blocking(input: &FindByIdBlockingInput) -> Result<Option<content::Document>, Status> {
    let infra = &input.infra;

    let conn = infra
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
    let read_hooks = RunnerReadHooks::new(&infra.hook_runner, &conn, user_doc, None);

    let ctx = ServiceContext::collection(&input.collection, &input.def)
        .infra(infra)
        .conn(&conn)
        .read_hooks(&read_hooks)
        .user(user_doc)
        .build();

    let find_input = FindByIdInput::builder(&input.id)
        .depth(input.depth)
        .select(input.select.as_deref())
        .locale_ctx(input.locale_ctx.as_ref())
        .registry(Some(&infra.registry))
        .use_draft(input.use_draft_version)
        .cache(Some(&*infra.cache))
        .include_deleted(input.include_deleted)
        .singleflight(Some(infra.populate_singleflight.clone()))
        .build();

    let doc = find_document_by_id(&ctx, &find_input).map_err(Status::from)?;

    match doc {
        Some(d) => Ok(Some(document_to_proto(&d, &input.collection))),
        None => Err(Status::not_found(format!(
            "Document '{}' not found in '{}'",
            input.id, input.collection
        ))),
    }
}

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Find a single document by ID with optional relationship population depth.
    pub(in crate::api::handlers) async fn find_by_id_impl(
        &self,
        request: Request<content::FindByIdRequest>,
    ) -> Result<Response<content::FindByIdResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let headers = self.metadata_headers(&metadata);
        let req = request.into_inner();
        let def = self.get_collection_def(&req.collection)?;

        let depth = query::clamp_depth(req.depth, self.default_depth, self.max_depth);

        let select = if req.select.is_empty() {
            None
        } else {
            Some(req.select.clone())
        };

        let locale_ctx =
            LocaleContext::from_locale_string(req.locale.as_deref(), &self.infra.locale_config)
                .map_err(|e| Status::invalid_argument(e.to_string()))?;

        let use_draft_version =
            req.draft.unwrap_or(false) && def.has_drafts() && def.has_versions();

        let include_deleted = req.trash.unwrap_or(false) && def.soft_delete;

        let input = FindByIdBlockingInput {
            infra: Arc::clone(&self.infra),
            db_kind: self.db_kind.clone(),
            collection: req.collection.clone(),
            id: req.id.clone(),
            def,
            token,
            headers,
            depth,
            select,
            locale_ctx,
            use_draft_version,
            include_deleted,
        };

        let result = task::spawn_blocking(move || find_by_id_blocking(&input))
            .await
            .inspect_err(|e| error!("Task error: {}", e))
            .map_err(|_| Status::internal("Internal error"))??;

        Ok(Response::new(content::FindByIdResponse {
            document: result,
        }))
    }
}
