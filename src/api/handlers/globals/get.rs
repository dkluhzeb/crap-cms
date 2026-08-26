//! `GetGlobal` handler — get the single document for a global definition.

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
    core::collection::GlobalDefinition,
    db::LocaleContext,
    service::{AppInfra, GetGlobalInput, RunnerReadHooks, ServiceContext, get_global_document},
};

/// Owned bundle for the `GetGlobal` spawn-blocking body. Process-stable
/// dependencies come from the shared [`AppInfra`]; the rest is per-call.
struct GetGlobalBlockingInput {
    infra: Arc<AppInfra>,
    headers: HashMap<String, String>,
    slug: String,
    def: GlobalDefinition,
    token: Option<String>,
    locale_ctx: Option<LocaleContext>,
    include_drafts: bool,
}

fn get_global_blocking(input: &GetGlobalBlockingInput) -> Result<content::Document, Status> {
    let infra = &input.infra;

    let conn = infra
        .pool
        .get()
        .inspect_err(|e| error!("GetGlobal pool error: {}", e))
        .map_err(|_| Status::internal("Internal error"))?;

    let auth_user = ContentService::resolve_auth_user(
        input.token.as_deref(),
        &input.headers,
        &*infra.token_provider,
        &infra.hook_runner,
        &infra.registry,
        &conn,
    )?;

    // Access check is handled by service::get_global_document
    let user_doc = auth_user.as_ref().map(|au| &au.user_doc);
    let read_hooks = RunnerReadHooks::new(&infra.hook_runner, &conn, user_doc, None);

    let ctx = ServiceContext::global(&input.slug, &input.def)
        .infra(infra)
        .conn(&conn)
        .read_hooks(&read_hooks)
        .user(user_doc)
        .build();

    let get_input =
        GetGlobalInput::new(input.locale_ctx.as_ref(), None).include_drafts(input.include_drafts);

    let doc = get_global_document(&ctx, &get_input).map_err(Status::from)?;

    Ok(document_to_proto(&doc, &input.slug))
}

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Get the single document for a global definition.
    pub(in crate::api::handlers) async fn get_global_impl(
        &self,
        request: Request<content::GetGlobalRequest>,
    ) -> Result<Response<content::GetGlobalResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let headers = self.metadata_headers(&metadata);
        let req = request.into_inner();
        let def = self.get_global_def(&req.slug)?;

        let locale_ctx =
            LocaleContext::from_locale_string(req.locale.as_deref(), &self.locale_config)
                .map_err(|e| Status::invalid_argument(e.to_string()))?;

        let input = GetGlobalBlockingInput {
            infra: Arc::clone(&self.infra),
            slug: req.slug.clone(),
            def,
            token,
            headers,
            locale_ctx,
            include_drafts: req.draft.unwrap_or(false),
        };

        let proto_doc = task::spawn_blocking(move || get_global_blocking(&input))
            .await
            .inspect_err(|e| error!("GetGlobal task error: {}", e))
            .map_err(|_| Status::internal("Internal error"))??;

        Ok(Response::new(content::GetGlobalResponse {
            document: Some(proto_doc),
        }))
    }
}
