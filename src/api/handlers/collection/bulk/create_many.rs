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
            proto::{data_map_to_json_map, document_to_proto},
        },
    },
    config::PasswordPolicy,
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
    password_policy: PasswordPolicy,
    run_hooks: bool,
    draft: bool,
    bulk_max_documents: i64,
    events: bool,
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
        .emit_events(input.events)
        .cache(input.cache)
        .email_ctx(input.email_ctx)
        .password_policy(Some(&input.password_policy))
        .build();

    let opts = CreateManyOptions {
        run_hooks: input.run_hooks,
        draft: input.draft,
        max_documents: input.bulk_max_documents,
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

        // Auth collections: split each item's `password` off so the service
        // create chokepoint validates it against the password policy and hashes
        // it — parity with single Create and Lua `create_many`. Bulk create is
        // per-item (distinct passwords per user), so seeding auth users with
        // policed passwords in one transaction is a legitimate operation; only
        // `update_many` (a broadcast that would set one password on many rows)
        // rejects a password. A non-auth collection keeps a legitimate
        // `password` field as ordinary data.
        let is_auth = def.is_auth_collection();
        let mut items: Vec<CreateManyItem> = Vec::with_capacity(req.documents.len());
        for s in &req.documents {
            let mut map = data_map_to_json_map(s);

            let password = if is_auth {
                map.remove("password")
                    .and_then(|v| v.as_str().map(std::string::ToString::to_string))
            } else {
                None
            };

            items.push(CreateManyItem {
                data: map.into(),
                password,
            });
        }

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
            password_policy: self.password_policy.clone(),
            run_hooks: req.hooks.unwrap_or(true),
            draft: req.draft.unwrap_or(false),
            bulk_max_documents: self.server_config.bulk_max_documents,
            events: req.events.unwrap_or(false),
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
