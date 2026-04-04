//! GetGlobal handler — get the single document for a global definition.

use std::collections::HashMap;

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::{error, warn};

use crate::{
    api::{
        content,
        service::{
            ContentService, collection::helpers::strip_denied_proto_fields,
            convert::document_to_proto,
        },
    },
    db::{AccessResult, LocaleContext, query},
    hooks::lifecycle::AfterReadCtx,
};

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Get the single document for a global definition.
    pub(in crate::api::service) async fn get_global_impl(
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
        let hooks = def.hooks.clone();
        let def_fields = def.fields.clone();
        let fields = def_fields.clone();
        let slug = req.slug.clone();

        let proto_doc = task::spawn_blocking(move || -> Result<_, Status> {
            let mut conn = pool.get().map_err(|e| {
                error!("GetGlobal pool error: {}", e);
                Status::internal("Internal error")
            })?;

            let auth_user =
                ContentService::resolve_auth_user(token, &*token_provider, &registry, &conn)?;

            let access_result = ContentService::check_access_blocking(
                def.access.read.as_deref(),
                &auth_user,
                None,
                None,
                &runner,
                &mut conn,
            )?;

            if matches!(access_result, AccessResult::Denied) {
                return Err(Status::permission_denied("Read access denied"));
            }

            runner
                .fire_before_read(&hooks, &slug, "get_global", HashMap::new())
                .map_err(|e| {
                    error!("GetGlobal hook error: {}", e);
                    Status::internal("Internal error")
                })?;

            let doc = query::get_global(&conn, &slug, &def, locale_ctx.as_ref()).map_err(|e| {
                error!("GetGlobal query error: {}", e);
                Status::internal("Internal error")
            })?;

            let ar_ctx = AfterReadCtx {
                hooks: &hooks,
                fields: &fields,
                collection: &slug,
                operation: "get_global",
                user: auth_user.as_ref().map(|au| &au.user_doc),
                ui_locale: None,
            };

            let doc = runner.apply_after_read(&ar_ctx, doc);
            let mut proto_doc = document_to_proto(&doc, &slug);

            let user_doc = auth_user.as_ref().map(|au| &au.user_doc);
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
        .map_err(|e| {
            error!("GetGlobal task error: {}", e);
            Status::internal("Internal error")
        })??;

        Ok(Response::new(content::GetGlobalResponse {
            document: Some(proto_doc),
        }))
    }
}
