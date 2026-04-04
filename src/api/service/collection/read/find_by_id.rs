//! FindByID handler — fetch a single document by ID.

use std::{
    collections::{HashMap, HashSet},
    slice,
};

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{
        content,
        service::{ContentService, collection::helpers::map_db_error, convert::document_to_proto},
    },
    core::upload,
    db::{AccessResult, LocaleContext, ops, query},
    hooks::lifecycle::AfterReadCtx,
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
        let hooks = def.hooks.clone();
        let fields = def.fields.clone();
        let collection = req.collection.clone();
        let id = req.id.clone();
        let def_fields = def.fields.clone();
        let pop_cache = self.cache.clone();
        let def_owned = def;

        let result = task::spawn_blocking(move || -> Result<_, Status> {
            let mut conn = pool.get().map_err(|e| map_db_error(e, "Pool", &db_kind))?;

            let auth_user =
                ContentService::resolve_auth_user(token, &*token_provider, &registry, &conn)?;

            let access_result = ContentService::check_access_blocking(
                def_owned.access.read.as_deref(),
                &auth_user,
                Some(&id),
                None,
                &runner,
                &mut conn,
            )?;

            if matches!(access_result, AccessResult::Denied) {
                return Err(Status::permission_denied("Read access denied"));
            }

            let access_constraints = if let AccessResult::Constrained(ref filters) = access_result {
                Some(filters.clone())
            } else {
                None
            };

            runner
                .fire_before_read(&hooks, &collection, "find_by_id", HashMap::new())
                .map_err(|e| map_db_error(e, "Query error", &db_kind))?;

            let mut doc = ops::find_by_id_full(
                &conn,
                &collection,
                &def_owned,
                &id,
                locale_ctx.as_ref(),
                access_constraints,
                use_draft_version,
            )
            .map_err(|e| map_db_error(e, "Query error", &db_kind))?;

            if let Some(ref mut d) = doc
                && let Some(ref upload_config) = def_owned.upload
                && upload_config.enabled
            {
                upload::assemble_sizes_object(d, upload_config);
            }

            let ar_ctx = AfterReadCtx {
                hooks: &hooks,
                fields: &fields,
                collection: &collection,
                operation: "find_by_id",
                user: auth_user.as_ref().map(|au| &au.user_doc),
                ui_locale: None,
            };

            let mut doc = doc.map(|d| runner.apply_after_read(&ar_ctx, d));
            let select_slice = select.as_deref();

            if depth > 0
                && let Some(ref mut d) = doc
            {
                let mut visited = HashSet::new();
                let cache_ref = &*pop_cache;
                let pop_ctx =
                    query::PopulateContext::new(&conn, &registry, &collection, &def_owned);
                let mut pop_opts = query::PopulateOpts::new(depth);

                if let Some(s) = select_slice {
                    pop_opts = pop_opts.select(s);
                }

                if let Some(ref lc) = locale_ctx {
                    pop_opts = pop_opts.locale_ctx(lc);
                }

                query::populate_relationships_cached(
                    &pop_ctx,
                    d,
                    &mut visited,
                    &pop_opts,
                    cache_ref,
                )
                .map_err(|e| map_db_error(e, "Query error", &db_kind))?;
            }

            if let Some(ref sel) = select
                && let Some(ref mut d) = doc
            {
                query::apply_select_to_document(d, sel);
            }

            match doc {
                Some(d) => {
                    let mut proto_doc = document_to_proto(&d, &collection);
                    let user_doc = auth_user.as_ref().map(|au| &au.user_doc);

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
        .map_err(|e| {
            error!("Task error: {}", e);
            Status::internal("Internal error")
        })??;

        Ok(Response::new(content::FindByIdResponse {
            document: result,
        }))
    }
}
