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
    core::{CollectionDefinition, DocumentFields},
    db::LocaleContext,
    service::{self, AppInfra, ServiceContext, ServiceError, WriteInput},
};

/// Owned bundle for the `Update` spawn-blocking body. Process-stable
/// dependencies come from the shared [`AppInfra`]; the rest is per-call.
struct UpdateBlockingInput {
    infra: Arc<AppInfra>,
    headers: HashMap<String, String>,
    db_kind: String,
    collection: String,
    id: String,
    def: CollectionDefinition,
    token: Option<String>,
    data: DocumentFields,
    password: Option<String>,
    locale_ctx: Option<LocaleContext>,
    draft: bool,
    events: bool,
}

fn update_blocking(input: UpdateBlockingInput) -> Result<content::Document, Status> {
    let conn = input
        .infra
        .pool
        .get()
        .map_err(|e| Status::from(ServiceError::classify(e, &input.db_kind)))?;

    let auth_user = ContentService::resolve_auth_user(
        input.token.as_deref(),
        &input.headers,
        &*input.infra.token_provider,
        &input.infra.hook_runner,
        &input.infra.registry,
        &conn,
    )?;

    // Field write access is now checked inside service::update_document_in_conn
    // via WriteHooks::field_write_denied (using the transaction connection).

    let user_doc = auth_user.as_ref().map(|au| au.user_doc.clone());
    let ui_locale = auth_user.as_ref().map(|au| au.ui_locale.clone());
    let write_input = WriteInput::builder(input.data)
        .password(input.password.as_deref())
        .locale_ctx(input.locale_ctx.as_ref())
        .draft(input.draft)
        .ui_locale(ui_locale)
        .build();

    let ctx = ServiceContext::collection(&input.collection, &input.def)
        .infra(&input.infra)
        .user(user_doc.as_ref())
        .emit_events(input.events)
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
            &self.infra.password_policy,
            true,
        )?;

        let locale_ctx =
            LocaleContext::from_locale_string(req.locale.as_deref(), &self.infra.locale_config)
                .map_err(|e| Status::invalid_argument(e.to_string()))?;

        let input = UpdateBlockingInput {
            infra: Arc::clone(&self.infra),
            db_kind: self.db_kind.clone(),
            collection: req.collection.clone(),
            id: req.id.clone(),
            def,
            token,
            headers,
            data,
            password,
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
