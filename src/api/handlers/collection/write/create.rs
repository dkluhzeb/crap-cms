//! Create handler — create a new document in a collection.

use std::sync::Arc;

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{
        content,
        handlers::{
            ContentService,
            collection::helpers::extract_auth_password,
            convert::{document_to_proto, prost_struct_to_json_map},
        },
    },
    core::{
        CollectionDefinition, DocumentFields, Registry, SharedCache, SharedEventTransport,
        SharedTokenProvider,
    },
    db::{DbPool, LocaleContext},
    hooks::HookRunner,
    service::{self, EmailContext, ServiceContext, ServiceError, WriteInput},
};

/// Owned bundle for the `Create` spawn-blocking body.
struct CreateBlockingInput {
    pool: DbPool,
    runner: HookRunner,
    token_provider: SharedTokenProvider,
    registry: Arc<Registry>,
    db_kind: String,
    event_transport: Option<SharedEventTransport>,
    cache: Option<SharedCache>,
    collection: String,
    def: CollectionDefinition,
    email_ctx: Option<EmailContext>,
    token: Option<String>,
    data: DocumentFields,
    password: Option<String>,
    locale_ctx: Option<LocaleContext>,
    draft: bool,
}

fn create_blocking(input: CreateBlockingInput) -> Result<content::Document, Status> {
    let conn = input
        .pool
        .get()
        .map_err(|e| Status::from(ServiceError::classify(e, &input.db_kind)))?;

    let auth_user = ContentService::resolve_auth_user(
        input.token,
        &*input.token_provider,
        &input.registry,
        &conn,
    )?;

    let user_doc = auth_user.as_ref().map(|au| au.user_doc.clone());
    let auth_user_ui_locale = auth_user.as_ref().map(|au| au.ui_locale.clone());
    let ui_locale = user_doc.as_ref().and_then(|_| auth_user_ui_locale.clone());

    let ctx = ServiceContext::collection(&input.collection, &input.def)
        .pool(&input.pool)
        .runner(&input.runner)
        .user(user_doc.as_ref())
        .event_transport(input.event_transport)
        .cache(input.cache)
        .email_ctx(input.email_ctx)
        .build();

    let (doc, _req_context) = service::create_document(
        &ctx,
        WriteInput::builder(input.data)
            .password(input.password.as_deref())
            .locale_ctx(input.locale_ctx.as_ref())
            .draft(input.draft)
            .ui_locale(ui_locale)
            .build(),
    )
    .map_err(|e| Status::from(e.reclassify(&input.db_kind)))?;

    Ok(document_to_proto(&doc, &input.collection))
}

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Create a new document, running before/after hooks within a transaction.
    pub(in crate::api::handlers) async fn create_impl(
        &self,
        request: Request<content::CreateRequest>,
    ) -> Result<Response<content::CreateResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let req = request.into_inner();
        let def = self.get_collection_def(&req.collection)?;

        let mut data: DocumentFields = req
            .data
            .map(|s| prost_struct_to_json_map(&s))
            .unwrap_or_default()
            .into();

        let password = extract_auth_password(
            &mut data,
            def.is_auth_collection(),
            &self.password_policy,
            false,
        )?;

        let locale_ctx =
            LocaleContext::from_locale_string(req.locale.as_deref(), &self.locale_config)
                .map_err(|e| Status::invalid_argument(e.to_string()))?;

        let input = CreateBlockingInput {
            pool: self.pool.clone(),
            runner: self.hook_runner.clone(),
            token_provider: self.token_provider.clone(),
            registry: self.registry.clone(),
            db_kind: self.db_kind.clone(),
            event_transport: self.event_transport.clone(),
            cache: Some(self.cache.clone()),
            collection: req.collection.clone(),
            def,
            email_ctx: Some(self.email_context()),
            token,
            data,
            password,
            locale_ctx,
            draft: req.draft.unwrap_or(false),
        };

        let proto_doc = task::spawn_blocking(move || create_blocking(input))
            .await
            .inspect_err(|e| error!("Task error: {}", e))
            .map_err(|_| Status::internal("Internal error"))??;

        Ok(Response::new(content::CreateResponse {
            document: Some(proto_doc),
        }))
    }
}
