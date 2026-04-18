//! Bulk UpdateMany RPC handler.

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use super::helpers::build_bulk_filters;

use crate::{
    api::{
        content,
        handlers::{
            ContentService,
            convert::{prost_struct_to_hashmap, prost_struct_to_json_map},
        },
    },
    db::{AccessResult, LocaleContext},
    service::{self, ServiceContext, ServiceError, UpdateManyOptions},
};

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Bulk update matching documents. Runs per-document lifecycle hooks by default.
    pub(in crate::api::handlers) async fn update_many_impl(
        &self,
        request: Request<content::UpdateManyRequest>,
    ) -> Result<Response<content::UpdateManyResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let req = request.into_inner();
        let def = self.get_collection_def(&req.collection)?;

        let mut join_data = req
            .data
            .as_ref()
            .map(prost_struct_to_json_map)
            .unwrap_or_default();

        let mut data = req
            .data
            .map(|s| prost_struct_to_hashmap(&s))
            .unwrap_or_default();

        if def.is_auth_collection() && data.contains_key("password") {
            return Err(Status::invalid_argument(
                "Password updates are not supported in UpdateMany. Use Update for individual documents.",
            ));
        }

        if def.is_auth_collection() {
            data.remove("password");
            join_data.remove("password");
        }

        let locale_ctx =
            LocaleContext::from_locale_string(req.locale.as_deref(), &self.locale_config)
                .map_err(|e| Status::invalid_argument(e.to_string()))?;
        let run_hooks = req.hooks.unwrap_or(true);
        let draft = req.draft.unwrap_or(false);

        let pool = self.pool.clone();
        let hook_runner = self.hook_runner.clone();
        let token_provider = self.token_provider.clone();
        let registry = self.registry.clone();
        let db_kind = self.db_kind.clone();
        let collection = req.collection.clone();
        let req_where = req.r#where.clone();
        let def_owned = def;
        let locale_config = self.locale_config.clone();
        let event_transport = self.event_transport.clone();
        let cache = Some(self.cache.clone());

        let modified = task::spawn_blocking(move || -> Result<i64, Status> {
            let mut conn = pool
                .get()
                .map_err(|e| Status::from(ServiceError::classify(e, &db_kind)))?;

            let auth_user =
                ContentService::resolve_auth_user(token, &*token_provider, &registry, &conn)?;

            let read_access = ContentService::check_access_blocking(
                def_owned.access.read.as_deref(),
                &auth_user,
                None,
                None,
                &hook_runner,
                &mut conn,
            )?;

            if matches!(read_access, AccessResult::Denied) {
                return Err(Status::permission_denied("Read access denied"));
            }

            drop(conn);

            let filters = build_bulk_filters(
                &collection,
                &def_owned,
                &read_access,
                req_where.as_deref(),
                !draft,
            )?;

            let user_doc = auth_user.as_ref().map(|au| &au.user_doc);

            let ctx = ServiceContext::collection(&collection, &def_owned)
                .pool(&pool)
                .runner(&hook_runner)
                .user(user_doc)
                .event_transport(event_transport)
                .cache(cache)
                .build();

            let update_opts = UpdateManyOptions {
                locale_ctx: locale_ctx.as_ref(),
                run_hooks,
                draft,
                ui_locale: None,
            };

            let result = service::update_many(
                &ctx,
                filters,
                data,
                &join_data,
                &locale_config,
                &update_opts,
            )
            .map_err(|e| Status::from(e.reclassify(&db_kind)))?;

            Ok(result.modified)
        })
        .await
        .inspect_err(|e| error!("Task error: {}", e))
        .map_err(|_| Status::internal("Internal error"))??;

        Ok(Response::new(content::UpdateManyResponse { modified }))
    }
}
