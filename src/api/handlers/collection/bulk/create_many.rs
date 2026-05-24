//! Bulk `CreateMany` RPC handler.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{
        content,
        handlers::{
            ContentService,
            proto::{document_to_proto, prost_struct_to_json_map},
        },
    },
    core::{
        CollectionDefinition, Registry, SharedCache, SharedEventTransport, SharedTokenProvider,
    },
    db::DbPool,
    hooks::HookRunner,
    service::{
        self, CreateManyItem, CreateManyOptions, EmailContext, ServiceContext, ServiceError,
    },
};

/// Owned bundle for the `CreateMany` spawn-blocking body.
struct CreateManyBlockingInput {
    pool: DbPool,
    hook_runner: HookRunner,
    headers: HashMap<String, String>,
    token_provider: SharedTokenProvider,
    registry: Arc<Registry>,
    db_kind: String,
    collection: String,
    def: CollectionDefinition,
    event_transport: Option<SharedEventTransport>,
    cache: Option<SharedCache>,
    email_ctx: Option<EmailContext>,
    token: Option<String>,
    items: Vec<CreateManyItem>,
    run_hooks: bool,
    draft: bool,
}

fn create_many_blocking(
    input: CreateManyBlockingInput,
) -> Result<(i64, Vec<content::Document>), Status> {
    let conn = input
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

    let ctx = ServiceContext::collection(&input.collection, &input.def)
        .pool(&input.pool)
        .runner(&input.hook_runner)
        .user(user_doc)
        .event_transport(input.event_transport)
        .cache(input.cache)
        .email_ctx(input.email_ctx)
        .build();

    let opts = CreateManyOptions {
        run_hooks: input.run_hooks,
        draft: input.draft,
    };

    let result = service::create_many(&ctx, &input.items, &opts)
        .map_err(|e| Status::from(e.reclassify(&input.db_kind)))?;

    let proto_docs: Vec<content::Document> = result
        .documents
        .iter()
        .map(|doc| document_to_proto(doc, &input.collection))
        .collect();

    Ok((result.created, proto_docs))
}

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Bulk create multiple documents. Runs per-document lifecycle hooks by default.
    pub(in crate::api::handlers) async fn create_many_impl(
        &self,
        request: Request<content::CreateManyRequest>,
    ) -> Result<Response<content::CreateManyResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let headers = self.metadata_headers(&metadata);
        let req = request.into_inner();
        let def = self.get_collection_def(&req.collection)?;

        let items: Vec<CreateManyItem> = req
            .documents
            .iter()
            .map(|s| CreateManyItem {
                data: prost_struct_to_json_map(s).into(),
                password: None,
            })
            .collect();

        let input = CreateManyBlockingInput {
            pool: self.pool.clone(),
            hook_runner: self.hook_runner.clone(),
            token_provider: self.token_provider.clone(),
            registry: Arc::clone(&self.registry),
            db_kind: self.db_kind.clone(),
            collection: req.collection.clone(),
            def,
            event_transport: self.event_transport.clone(),
            cache: Some(self.cache.clone()),
            email_ctx: Some(self.email_context()),
            token,
            headers,
            items,
            run_hooks: req.hooks.unwrap_or(true),
            draft: req.draft.unwrap_or(false),
        };

        let result = task::spawn_blocking(move || create_many_blocking(input))
            .await
            .inspect_err(|e| error!("Task error: {}", e))
            .map_err(|_| Status::internal("Internal error"))??;

        Ok(Response::new(content::CreateManyResponse {
            created: result.0,
            documents: result.1,
        }))
    }
}
