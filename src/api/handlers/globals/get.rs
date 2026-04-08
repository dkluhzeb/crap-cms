//! GetGlobal handler — get the single document for a global definition.

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::{error, warn};

use crate::{
    api::{
        content,
        handlers::{
            ContentService, collection::helpers::strip_denied_proto_fields,
            convert::document_to_proto,
        },
    },
    db::LocaleContext,
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
            LocaleContext::from_locale_string(req.locale.as_deref(), &self.locale_config);

        let pool = self.pool.clone();
        let runner = self.hook_runner.clone();
        let token_provider = self.token_provider.clone();
        let registry = self.registry.clone();
        let def_fields = def.fields.clone();
        let slug = req.slug.clone();
        let def_owned = def;

        let proto_doc = task::spawn_blocking(move || -> Result<_, Status> {
            let mut conn = pool.get().map_err(|e| {
                error!("GetGlobal pool error: {}", e);
                Status::internal("Internal error")
            })?;

            let auth_user =
                ContentService::resolve_auth_user(token, &*token_provider, &registry, &conn)?;

            // Access check is handled by service::get_global_document
            let user_doc = auth_user.as_ref().map(|au| &au.user_doc);
            let read_hooks = crate::service::RunnerReadHooks {
                runner: &runner,
                conn: &conn,
            };

            let doc = crate::service::get_global_document(
                &conn,
                &read_hooks,
                &slug,
                &def_owned,
                locale_ctx.as_ref(),
                user_doc,
                None,
            )
            .map_err(Status::from)?;

            let mut proto_doc = document_to_proto(&doc, &slug);

            // Proto-level field stripping (defense in depth — service already stripped at JSON level)
            let tx = conn.transaction().map_err(|e| {
                error!("Field access check tx error: {}", e);
                Status::internal("Internal error")
            })?;
            let denied = runner.check_field_read_access(&def_fields, user_doc, &tx);
            if let Err(e) = tx.commit() {
                warn!("tx commit failed: {e}");
            }
            strip_denied_proto_fields(&mut proto_doc, &denied);

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
