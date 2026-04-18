//! UpdateGlobal handler — update a global's document.

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{
        content,
        handlers::{
            ContentService,
            convert::{document_to_proto, prost_struct_to_hashmap, prost_struct_to_json_map},
        },
    },
    db::LocaleContext,
    service::{self, ServiceContext, WriteInput},
};

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Update a global's document, running hooks within a transaction.
    pub(in crate::api::handlers) async fn update_global_impl(
        &self,
        request: Request<content::UpdateGlobalRequest>,
    ) -> Result<Response<content::UpdateGlobalResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let req = request.into_inner();
        let def = self.get_global_def(&req.slug)?;

        let join_data = req
            .data
            .as_ref()
            .map(prost_struct_to_json_map)
            .unwrap_or_default();

        let data = req
            .data
            .map(|s| prost_struct_to_hashmap(&s))
            .unwrap_or_default();

        let locale_ctx =
            LocaleContext::from_locale_string(req.locale.as_deref(), &self.locale_config)
                .map_err(|e| Status::invalid_argument(e.to_string()))?;

        let pool = self.pool.clone();
        let runner = self.hook_runner.clone();
        let token_provider = self.token_provider.clone();
        let registry = self.registry.clone();
        let event_transport = self.event_transport.clone();
        let cache = Some(self.cache.clone());
        let slug = req.slug.clone();
        let def_owned = def;

        let (proto_doc, _auth_user) = task::spawn_blocking(move || -> Result<_, Status> {
            let conn = pool.get().map_err(|e| {
                error!("UpdateGlobal pool error: {}", e);
                Status::internal("Internal error")
            })?;

            let auth_user =
                ContentService::resolve_auth_user(token, &*token_provider, &registry, &conn)?;

            // Access control (collection + field level) is checked inside
            // service::update_global_document via WriteHooks.

            let user_doc = auth_user.as_ref().map(|au| au.user_doc.clone());
            let ui_locale = auth_user.as_ref().map(|au| au.ui_locale.clone());
            drop(conn);

            let ctx = ServiceContext::global(&slug, &def_owned)
                .pool(&pool)
                .runner(&runner)
                .user(user_doc.as_ref())
                .event_transport(event_transport)
                .cache(cache)
                .build();

            let (doc, _req_context) = service::update_global_document(
                &ctx,
                WriteInput::builder(data, &join_data)
                    .locale_ctx(locale_ctx.as_ref())
                    .ui_locale(ui_locale)
                    .build(),
            )
            .map_err(|e| {
                error!("UpdateGlobal error: {}", e);
                Status::internal("Internal error")
            })?;

            let proto_doc = document_to_proto(&doc, &slug);

            Ok((proto_doc, auth_user))
        })
        .await
        .inspect_err(|e| error!("UpdateGlobal task error: {}", e))
        .map_err(|_| Status::internal("Internal error"))??;

        Ok(Response::new(content::UpdateGlobalResponse {
            document: Some(proto_doc),
        }))
    }
}
