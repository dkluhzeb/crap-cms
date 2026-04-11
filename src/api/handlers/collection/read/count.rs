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
    service::{
        CountDocumentsInput, RunnerReadHooks, ServiceContext, ServiceError, count_documents,
    },
};

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
            LocaleContext::from_locale_string(req.locale.as_deref(), &self.locale_config)
                .map_err(|e| Status::invalid_argument(e.to_string()))?;

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
            let conn = pool
                .get()
                .map_err(|e| Status::from(ServiceError::classify(e, &db_kind)))?;

            let auth_user =
                ContentService::resolve_auth_user(token, &*token_provider, &registry, &conn)?;

            // Build caller-side filters (where + draft). Access check is handled by
            // service::count_documents via ReadHooks.
            let filters = FilterBuilder::new(&def_owned.fields, &AccessResult::Allowed)
                .where_json(req_where.as_deref())
                .draft_filter(has_drafts, !draft.unwrap_or(false))
                .build()?;

            let user_doc = auth_user.as_ref().map(|au| &au.user_doc);
            let read_hooks = RunnerReadHooks::new(&runner, &conn);

            let ctx = ServiceContext::collection(&collection, &def_owned)
                .pool(&pool)
                .conn(&conn)
                .read_hooks(&read_hooks)
                .user(user_doc)
                .build();

            let input = CountDocumentsInput::builder(&filters)
                .locale_ctx(locale_ctx.as_ref())
                .search(search.as_deref())
                .build();

            count_documents(&ctx, &input).map_err(Status::from)
        })
        .await
        .inspect_err(|e| error!("Task error: {}", e))
        .map_err(|_| Status::internal("Internal error"))??;

        Ok(Response::new(content::CountResponse { count }))
    }
}
