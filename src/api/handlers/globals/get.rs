//! GetGlobal handler — get the single document for a global definition.

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{
        content,
        handlers::{ContentService, convert::document_to_proto},
    },
    db::LocaleContext,
    service::{GetGlobalInput, RunnerReadHooks, ServiceContext, get_global_document},
};

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Get the single document for a global definition.
    pub(in crate::api::handlers) async fn get_global_impl(
        &self,
        request: Request<content::GetGlobalRequest>,
    ) -> Result<Response<content::GetGlobalResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let req = request.into_inner();
        let def = self.get_global_def(&req.slug)?;

        let locale_ctx =
            LocaleContext::from_locale_string(req.locale.as_deref(), &self.locale_config)
                .map_err(|e| Status::invalid_argument(e.to_string()))?;

        let pool = self.pool.clone();
        let runner = self.hook_runner.clone();
        let token_provider = self.token_provider.clone();
        let registry = self.registry.clone();
        let slug = req.slug.clone();
        let def_owned = def;

        let proto_doc = task::spawn_blocking(move || -> Result<_, Status> {
            let conn = pool.get().map_err(|e| {
                error!("GetGlobal pool error: {}", e);
                Status::internal("Internal error")
            })?;

            let auth_user =
                ContentService::resolve_auth_user(token, &*token_provider, &registry, &conn)?;

            // Access check is handled by service::get_global_document
            let user_doc = auth_user.as_ref().map(|au| &au.user_doc);
            let read_hooks = RunnerReadHooks::new(&runner, &conn);

            let ctx = ServiceContext::global(&slug, &def_owned)
                .pool(&pool)
                .conn(&conn)
                .read_hooks(&read_hooks)
                .user(user_doc)
                .build();

            let input = GetGlobalInput::new(locale_ctx.as_ref(), None);

            let doc = get_global_document(&ctx, &input).map_err(Status::from)?;

            let proto_doc = document_to_proto(&doc, &slug);

            Ok(proto_doc)
        })
        .await
        .inspect_err(|e| error!("GetGlobal task error: {}", e))
        .map_err(|_| Status::internal("Internal error"))??;

        Ok(Response::new(content::GetGlobalResponse {
            document: Some(proto_doc),
        }))
    }
}
