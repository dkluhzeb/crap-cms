//! Unpublish handler — revert a published document to draft status.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::task;
use tonic::{Response, Status};
use tracing::error;

use crate::{
    api::{
        content,
        handlers::{ContentService, proto::document_to_proto},
    },
    config::LocaleConfig,
    core::{
        CollectionDefinition, Registry, SharedCache, SharedEventTransport,
        SharedInvalidationTransport, SharedTokenProvider,
    },
    db::DbPool,
    hooks::HookRunner,
    service::{self, ServiceContext, ServiceError},
};

/// Owned bundle for the `Unpublish` spawn-blocking body.
struct UnpublishBlockingInput {
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
    /// Required by `unpublish_document_in_conn` to build a default
    /// `LocaleContext` for the raw read — without this, collections with
    /// `localized = true` fields fail with `no such column: <name>` because
    /// the SELECT references bare names instead of `__en`/`__de`.
    locale_config: LocaleConfig,
    token: Option<String>,
    events: bool,
}

fn unpublish_blocking(input: UnpublishBlockingInput) -> Result<content::Document, Status> {
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

    let user_doc = auth_user.as_ref().map(|au| au.user_doc.clone());

    drop(conn);

    let ctx = ServiceContext::collection(&input.collection, &input.def)
        .pool(&input.pool)
        .runner(&input.runner)
        .user(user_doc.as_ref())
        .event_transport(input.event_transport)
        .invalidation_transport(Some(input.invalidation_transport))
        .emit_events(input.events)
        .cache(input.cache)
        .locale_config(Some(&input.locale_config))
        .build();

    let doc = service::unpublish_document(&ctx, &input.id)
        .map_err(|e| Status::from(e.reclassify(&input.db_kind)))?;

    Ok(document_to_proto(&doc, &input.collection))
}

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Unpublish a document: set status to draft, create version snapshot.
    pub(in crate::api::handlers) async fn unpublish_impl(
        &self,
        token: Option<String>,
        headers: HashMap<String, String>,
        req: &content::UpdateRequest,
        def: &CollectionDefinition,
    ) -> Result<Response<content::UpdateResponse>, Status> {
        let input = UnpublishBlockingInput {
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
            def: def.clone(),
            locale_config: self.locale_config.clone(),
            token,
            headers,
            events: req.events.unwrap_or(true),
        };

        let proto_doc = task::spawn_blocking(move || unpublish_blocking(input))
            .await
            .inspect_err(|e| error!("Task error: {}", e))
            .map_err(|_| Status::internal("Internal error"))??;

        Ok(Response::new(content::UpdateResponse {
            document: Some(proto_doc),
        }))
    }
}
