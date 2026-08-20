//! Update handler — update an existing document by ID.

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
            collection::helpers::extract_auth_password,
            proto::{data_map_to_json_map, document_to_proto},
        },
    },
    config::PasswordPolicy,
    core::{
        CollectionDefinition, DocumentFields, Registry, SharedCache, SharedEventTransport,
        SharedInvalidationTransport, SharedTokenProvider,
    },
    db::{DbPool, LocaleContext},
    hooks::HookRunner,
    service::{self, ServiceContext, ServiceError, WriteInput},
};

/// Owned bundle for the `Update` spawn-blocking body.
struct UpdateBlockingInput {
    pool: DbPool,
    runner: HookRunner,
    headers: HashMap<String, String>,
    token_provider: SharedTokenProvider,
    registry: Arc<Registry>,
    db_kind: String,
    event_transport: Option<SharedEventTransport>,
    invalidation_transport: SharedInvalidationTransport,
    cache: Option<SharedCache>,
    collection: String,
    id: String,
    def: CollectionDefinition,
    token: Option<String>,
    data: DocumentFields,
    password: Option<String>,
    password_policy: PasswordPolicy,
    locale_ctx: Option<LocaleContext>,
    draft: bool,
    events: bool,
}

fn update_blocking(input: UpdateBlockingInput) -> Result<content::Document, Status> {
    let conn = input
        .pool
        .get()
        .map_err(|e| Status::from(ServiceError::classify(e, &input.db_kind)))?;

    let auth_user = ContentService::resolve_auth_user(
        input.token.as_deref(),
        &input.headers,
        &*input.token_provider,
        &input.runner,
        &input.registry,
        &conn,
    )?;

    // Field write access is now checked inside service::update_document_in_conn
    // via WriteHooks::field_write_denied (using the transaction connection).

    let user_doc = auth_user.as_ref().map(|au| au.user_doc.clone());
    let auth_user_ui_locale = auth_user.as_ref().map(|au| au.ui_locale.clone());
    let ui_locale = user_doc.as_ref().and_then(|_| auth_user_ui_locale.clone());
    let write_input = WriteInput::builder(input.data)
        .password(input.password.as_deref())
        .locale_ctx(input.locale_ctx.as_ref())
        .draft(input.draft)
        .ui_locale(ui_locale)
        .build();

    let ctx = ServiceContext::collection(&input.collection, &input.def)
        .pool(&input.pool)
        .runner(&input.runner)
        .user(user_doc.as_ref())
        .event_transport(input.event_transport)
        .invalidation_transport(Some(input.invalidation_transport))
        .emit_events(input.events)
        .cache(input.cache)
        .password_policy(Some(&input.password_policy))
        .build();

    let (doc, _req_context) = service::update_document(&ctx, &input.id, write_input)
        .map_err(|e| Status::from(e.reclassify(&input.db_kind)))?;

    Ok(document_to_proto(&doc, &input.collection))
}

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Update an existing document by ID, running before/after hooks within a transaction.
    pub(in crate::api::handlers) async fn update_impl(
        &self,
        request: Request<content::UpdateRequest>,
    ) -> Result<Response<content::UpdateResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let headers = self.metadata_headers(&metadata);
        let req = request.into_inner();
        let def = self.get_collection_def(&req.collection)?;

        // Route an unpublish request to the unpublish path regardless of
        // versioning: the shared service gate rejects unpublish on a
        // non-versioned collection (an explicit error), instead of silently
        // falling through to a normal update as the old `&& has_versions()`
        // short-circuit did. Harmonizes with the Lua surface's error.
        if req.unpublish.unwrap_or(false) {
            return self.unpublish_impl(token, headers, &req, &def).await;
        }

        let mut data: DocumentFields = req
            .data
            .map(|s| data_map_to_json_map(&s))
            .unwrap_or_default()
            .into();

        let password = extract_auth_password(
            &mut data,
            def.is_auth_collection(),
            &self.password_policy,
            true,
        )?;

        let locale_ctx =
            LocaleContext::from_locale_string(req.locale.as_deref(), &self.locale_config)
                .map_err(|e| Status::invalid_argument(e.to_string()))?;

        let input = UpdateBlockingInput {
            pool: self.pool.clone(),
            runner: self.hook_runner.clone(),
            token_provider: self.token_provider.clone(),
            registry: Arc::clone(&self.registry),
            db_kind: self.db_kind.clone(),
            event_transport: self.event_transport.clone(),
            invalidation_transport: self.invalidation_transport.clone(),
            cache: Some(self.cache.clone()),
            collection: req.collection.clone(),
            id: req.id.clone(),
            def,
            token,
            headers,
            data,
            password,
            password_policy: self.password_policy.clone(),
            locale_ctx,
            draft: req.draft.unwrap_or(false),
            events: req.events.unwrap_or(true),
        };

        let proto_doc = task::spawn_blocking(move || update_blocking(input))
            .await
            .inspect_err(|e| error!("Task error: {}", e))
            .map_err(|_| Status::internal("Internal error"))??;

        Ok(Response::new(content::UpdateResponse {
            document: Some(proto_doc),
        }))
    }
}
