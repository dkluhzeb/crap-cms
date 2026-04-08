//! UpdateGlobal handler — update a global's document.

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::{error, warn};

use crate::{
    api::{
        content,
        handlers::{
            ContentService,
            convert::{document_to_proto, prost_struct_to_hashmap, prost_struct_to_json_map},
        },
    },
    core::event::{EventOperation, EventTarget},
    db::{AccessResult, LocaleContext},
    hooks::lifecycle::PublishEventInput,
    service::{self, WriteInput},
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
            LocaleContext::from_locale_string(req.locale.as_deref(), &self.locale_config);

        let pool = self.pool.clone();
        let runner = self.hook_runner.clone();
        let token_provider = self.token_provider.clone();
        let registry = self.registry.clone();
        let slug = req.slug.clone();
        let def_owned = def;

        let (proto_doc, auth_user) = task::spawn_blocking(move || -> Result<_, Status> {
            let mut conn = pool.get().map_err(|e| {
                error!("UpdateGlobal pool error: {}", e);
                Status::internal("Internal error")
            })?;

            let auth_user =
                ContentService::resolve_auth_user(token, &*token_provider, &registry, &conn)?;

            let access_result = ContentService::check_access_blocking(
                def_owned.access.update.as_deref(),
                &auth_user,
                None,
                None,
                &runner,
                &mut conn,
            )?;

            if matches!(access_result, AccessResult::Denied) {
                return Err(Status::permission_denied("Update access denied"));
            }

            // Field write access is now checked inside service::update_global_core
            // via WriteHooks::field_write_denied (using the transaction connection).

            let user_doc = auth_user.as_ref().map(|au| au.user_doc.clone());
            let ui_locale = auth_user.as_ref().map(|au| au.ui_locale.clone());
            drop(conn);

            let (doc, _req_context) = service::update_global_document(
                &pool,
                &runner,
                &slug,
                &def_owned,
                WriteInput::builder(data, &join_data)
                    .locale_ctx(locale_ctx.as_ref())
                    .ui_locale(ui_locale)
                    .build(),
                user_doc.as_ref(),
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

        if let Err(e) = self.cache.clear() {
            warn!("Cache clear failed: {:#}", e);
        }

        {
            let def = self.get_global_def(&req.slug);
            let (hooks, live) = match &def {
                Ok(d) => (d.hooks.clone(), d.live.clone()),
                Err(_) => (Default::default(), None),
            };

            self.hook_runner.publish_event(
                &self.event_bus,
                &hooks,
                live.as_ref(),
                PublishEventInput::builder(EventTarget::Global, EventOperation::Update)
                    .collection(req.slug.clone())
                    .document_id(proto_doc.id.clone())
                    .edited_by(Self::event_user_from(&auth_user))
                    .build(),
            );
        }

        Ok(Response::new(content::UpdateGlobalResponse {
            document: Some(proto_doc),
        }))
    }
}
