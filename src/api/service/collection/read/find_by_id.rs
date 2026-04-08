//! FindByID handler — fetch a single document by ID.

use std::slice;

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{
        content,
        service::{ContentService, collection::helpers::map_db_error, convert::document_to_proto},
    },
    db::LocaleContext,
};

use crate::api::service::collection::helpers::strip_read_denied_proto_fields;

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Find a single document by ID with optional relationship population depth.
    pub(in crate::api::service) async fn find_by_id_impl(
        &self,
        request: Request<content::FindByIdRequest>,
    ) -> Result<Response<content::FindByIdResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let req = request.into_inner();
        let def = self.get_collection_def(&req.collection)?;

        let depth = req
            .depth
            .unwrap_or(self.default_depth)
            .max(0)
            .min(self.max_depth);

        let select = if req.select.is_empty() {
            None
        } else {
            Some(req.select.clone())
        };

        let locale_ctx =
            LocaleContext::from_locale_string(req.locale.as_deref(), &self.locale_config);

        let use_draft_version =
            req.draft.unwrap_or(false) && def.has_drafts() && def.has_versions();

        let pool = self.pool.clone();
        let runner = self.hook_runner.clone();
        let token_provider = self.token_provider.clone();
        let registry = self.registry.clone();
        let db_kind = self.db_kind.clone();
        let collection = req.collection.clone();
        let id = req.id.clone();
        let def_fields = def.fields.clone();
        let pop_cache = self.cache.clone();
        let def_owned = def;

        let result = task::spawn_blocking(move || -> Result<_, Status> {
            let mut conn = pool.get().map_err(|e| map_db_error(e, "Pool", &db_kind))?;

            let auth_user =
                ContentService::resolve_auth_user(token, &*token_provider, &registry, &conn)?;

            // Access check is handled by service::find_document_by_id
            let user_doc = auth_user.as_ref().map(|au| &au.user_doc);
            let read_hooks = crate::service::RunnerReadHooks {
                runner: &runner,
                conn: &conn,
            };
            let read_opts = crate::service::ReadOptions {
                depth,
                hydrate: true,
                select: select.as_deref(),
                locale_ctx: locale_ctx.as_ref(),
                registry: Some(&registry),
                user: user_doc,
                ui_locale: None,
                use_draft: use_draft_version,
                cache: Some(&*pop_cache),
                ..Default::default()
            };

            let doc = crate::service::find_document_by_id(
                &conn,
                &read_hooks,
                &collection,
                &def_owned,
                &id,
                &read_opts,
            )
            .map_err(Status::from)?;

            match doc {
                Some(d) => {
                    let mut proto_doc = document_to_proto(&d, &collection);

                    strip_read_denied_proto_fields(
                        slice::from_mut(&mut proto_doc),
                        &mut conn,
                        &runner,
                        &def_fields,
                        user_doc,
                    );

                    Ok(Some(proto_doc))
                }
                None => Err(Status::not_found(format!(
                    "Document '{}' not found in '{}'",
                    id, collection
                ))),
            }
        })
        .await
        .inspect_err(|e| error!("Task error: {}", e))
        .map_err(|_| Status::internal("Internal error"))??;

        Ok(Response::new(content::FindByIdResponse {
            document: result,
        }))
    }
}
