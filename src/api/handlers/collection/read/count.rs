//! Count handler — count documents matching filters.

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{
        content,
        handlers::{ContentService, collection::filter_builder::FilterBuilder},
    },
    db::{AccessResult, LocaleContext},
};

use crate::api::handlers::collection::helpers::map_db_error;

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Count documents matching filters (no per-document hooks).
    pub(in crate::api::handlers) async fn count_impl(
        &self,
        request: Request<content::CountRequest>,
    ) -> Result<Response<content::CountResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let req = request.into_inner();
        let def = self.get_collection_def(&req.collection)?;

        let locale_ctx =
            LocaleContext::from_locale_string(req.locale.as_deref(), &self.locale_config);

        let pool = self.pool.clone();
        let runner = self.hook_runner.clone();
        let token_provider = self.token_provider.clone();
        let registry = self.registry.clone();
        let db_kind = self.db_kind.clone();
        let collection = req.collection.clone();
        let req_where = req.r#where.clone();
        let has_drafts = def.has_drafts();
        let draft = req.draft;
        let search = req.search.clone();
        let def_owned = def;

        let count = task::spawn_blocking(move || -> Result<_, Status> {
            let conn = pool.get().map_err(|e| map_db_error(e, "Pool", &db_kind))?;

            let auth_user =
                ContentService::resolve_auth_user(token, &*token_provider, &registry, &conn)?;

            // Build caller-side filters (where + draft). Access check is handled by
            // service::count_documents via ReadHooks.
            let filters = FilterBuilder::new(&def_owned.fields, &AccessResult::Allowed)
                .where_json(req_where.as_deref())
                .draft_filter(has_drafts, !draft.unwrap_or(false))
                .build()?;

            let user_doc = auth_user.as_ref().map(|au| &au.user_doc);
            let read_hooks = crate::service::RunnerReadHooks {
                runner: &runner,
                conn: &conn,
            };

            crate::service::count_documents(
                &conn,
                &read_hooks,
                &collection,
                &def_owned,
                &filters,
                locale_ctx.as_ref(),
                search.as_deref(),
                false,
                user_doc,
            )
            .map_err(Status::from)
        })
        .await
        .inspect_err(|e| error!("Task error: {}", e))
        .map_err(|_| Status::internal("Internal error"))??;

        Ok(Response::new(content::CountResponse { count }))
    }
}
