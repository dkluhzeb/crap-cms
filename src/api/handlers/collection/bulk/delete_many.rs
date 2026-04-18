//! Bulk DeleteMany RPC handler.

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{content, handlers::ContentService},
    core::upload,
    db::AccessResult,
    service::{DeleteManyOptions, ServiceContext, ServiceError, delete_many},
};

use super::helpers::build_bulk_filters;

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Bulk delete matching documents. Runs per-document lifecycle hooks by default.
    pub(in crate::api::handlers) async fn delete_many_impl(
        &self,
        request: Request<content::DeleteManyRequest>,
    ) -> Result<Response<content::DeleteManyResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let req = request.into_inner();
        let mut def = self.get_collection_def(&req.collection)?;
        let run_hooks = req.hooks.unwrap_or(true);

        if req.force_hard_delete && def.soft_delete {
            def.soft_delete = false;
        }

        let pool = self.pool.clone();
        let hook_runner = self.hook_runner.clone();
        let token_provider = self.token_provider.clone();
        let registry = self.registry.clone();
        let db_kind = self.db_kind.clone();
        let collection = req.collection.clone();
        let req_where = req.r#where.clone();
        let storage = self.storage.clone();
        let locale_cfg = self.locale_config.clone();
        let def_owned = def;
        let invalidation_transport = self.invalidation_transport.clone();
        let event_transport = self.event_transport.clone();
        let cache = Some(self.cache.clone());

        let result = task::spawn_blocking(move || -> Result<_, Status> {
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
                true,
            )?;

            let user_doc = auth_user.as_ref().map(|au| &au.user_doc);

            let ctx = ServiceContext::collection(&collection, &def_owned)
                .pool(&pool)
                .runner(&hook_runner)
                .user(user_doc)
                .invalidation_transport(Some(invalidation_transport))
                .event_transport(event_transport)
                .cache(cache)
                .build();

            let delete_opts = DeleteManyOptions {
                run_hooks,
                ..Default::default()
            };

            let result = delete_many(&ctx, filters, &locale_cfg, &delete_opts)
                .map_err(|e| Status::from(e.reclassify(&db_kind)))?;

            for fields in &result.upload_fields_to_clean {
                upload::delete_upload_files(&*storage, fields);
            }

            Ok(result)
        })
        .await
        .inspect_err(|e| error!("Task error: {}", e))
        .map_err(|_| Status::internal("Internal error"))??;

        Ok(Response::new(content::DeleteManyResponse {
            deleted: result.hard_deleted,
            soft_deleted: result.soft_deleted,
            skipped: result.skipped,
        }))
    }
}
