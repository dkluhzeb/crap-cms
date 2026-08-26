//! `UpdateGlobal` handler — update a global's document.

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
    core::{DocumentFields, GlobalDefinition},
    db::LocaleContext,
    service::{self, AppInfra, ServiceContext, WriteInput},
};

/// Owned bundle for the `UpdateGlobal` spawn-blocking body. Process-stable
/// dependencies come from the shared [`AppInfra`]; the rest is per-call.
struct UpdateGlobalBlockingInput {
    infra: Arc<AppInfra>,
    headers: HashMap<String, String>,
    slug: String,
    def: GlobalDefinition,
    token: Option<String>,
    data: DocumentFields,
    locale_ctx: Option<LocaleContext>,
    events: bool,
}

/// Resolve auth, build the context, and run `update_global_document`.
fn update_global_blocking(input: UpdateGlobalBlockingInput) -> Result<content::Document, Status> {
    let conn = input
        .infra
        .pool
        .get()
        .inspect_err(|e| error!("UpdateGlobal pool error: {}", e))
        .map_err(|_| Status::internal("Internal error"))?;

    let auth_user = ContentService::resolve_auth_user(
        input.token.as_deref(),
        &input.headers,
        &*input.infra.token_provider,
        &input.infra.hook_runner,
        &input.infra.registry,
        &conn,
    )?;

    // Access control (collection + field level) is checked inside
    // service::update_global_document via WriteHooks.

    let user_doc = auth_user.as_ref().map(|au| au.user_doc.clone());
    let ui_locale = auth_user.as_ref().map(|au| au.ui_locale.clone());
    drop(conn);

    let ctx = ServiceContext::global(&input.slug, &input.def)
        .infra(&input.infra)
        .user(user_doc.as_ref())
        .emit_events(input.events)
        .build();

    let (doc, _req_context) = service::update_global_document(
        &ctx,
        WriteInput::builder(input.data)
            .locale_ctx(input.locale_ctx.as_ref())
            .ui_locale(ui_locale)
            .build(),
    )
    .inspect_err(|e| error!("UpdateGlobal error: {}", e))
    .map_err(Status::from)?;

    Ok(document_to_proto(&doc, &input.slug))
}

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Update a global's document, running hooks within a transaction.
    pub(in crate::api::handlers) async fn update_global_impl(
        &self,
        request: Request<content::UpdateGlobalRequest>,
    ) -> Result<Response<content::UpdateGlobalResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let headers = self.metadata_headers(&metadata);
        let req = request.into_inner();
        let def = self.get_global_def(&req.slug)?;

        let data: DocumentFields = req
            .data
            .map(|s| data_map_to_json_map(&s))
            .unwrap_or_default()
            .into();

        let locale_ctx =
            LocaleContext::from_locale_string(req.locale.as_deref(), &self.locale_config)
                .map_err(|e| Status::invalid_argument(e.to_string()))?;

        let input = UpdateGlobalBlockingInput {
            infra: Arc::clone(&self.infra),
            slug: req.slug.clone(),
            def,
            token,
            headers,
            data,
            locale_ctx,
            events: req.events.unwrap_or(true),
        };

        let proto_doc = task::spawn_blocking(move || update_global_blocking(input))
            .await
            .inspect_err(|e| error!("UpdateGlobal task error: {}", e))
            .map_err(|_| Status::internal("Internal error"))??;

        Ok(Response::new(content::UpdateGlobalResponse {
            document: Some(proto_doc),
        }))
    }
}
