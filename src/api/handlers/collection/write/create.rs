//! Create handler — create a new document in a collection.

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

/// Owned bundle for the `Create` spawn-blocking body. Process-stable
/// dependencies come from the shared [`AppInfra`]; the rest is per-call.
struct CreateBlockingInput {
    infra: Arc<AppInfra>,
    headers: HashMap<String, String>,
    db_kind: String,
    collection: String,
    def: CollectionDefinition,
    token: Option<String>,
    data: DocumentFields,
    password: Option<String>,
    locale_ctx: Option<LocaleContext>,
    draft: bool,
    events: bool,
}

fn create_blocking(input: CreateBlockingInput) -> Result<content::Document, Status> {
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

    let user_doc = auth_user.as_ref().map(|au| au.user_doc.clone());
    let ui_locale = auth_user.as_ref().map(|au| au.ui_locale.clone());

    // Move `data` out before borrowing `input.infra` for the context.
    let data = input.data;

    let ctx = ServiceContext::collection(&input.collection, &input.def)
        .infra(&input.infra)
        .user(user_doc.as_ref())
        .emit_events(input.events)
        .build();

    let (doc, _req_context) = service::create_document(
        &ctx,
        WriteInput::builder(data)
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
        let headers = self.metadata_headers(&metadata);
        let req = request.into_inner();
        let def = self.get_collection_def(&req.collection)?;

        let mut data: DocumentFields = req
            .data
            .map(|s| data_map_to_json_map(&s))
            .unwrap_or_default()
            .into();

        let password = extract_auth_password(
            &mut data,
            def.is_auth_collection(),
            &self.infra.password_policy,
            false,
        )?;

        let locale_ctx =
            LocaleContext::from_locale_string(req.locale.as_deref(), &self.infra.locale_config)
                .map_err(|e| Status::invalid_argument(e.to_string()))?;

        let input = CreateBlockingInput {
            infra: Arc::clone(&self.infra),
            db_kind: self.db_kind.clone(),
            collection: req.collection.clone(),
            def,
            token,
            headers,
            data,
            password,
            locale_ctx,
            draft: req.draft.unwrap_or(false),
            events: req.events.unwrap_or(true),
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
