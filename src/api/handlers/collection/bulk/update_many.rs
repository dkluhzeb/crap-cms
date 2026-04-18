//! Bulk UpdateMany RPC handler.

use anyhow::Context as _;
use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use super::helpers::{build_bulk_filters, check_per_doc_access};

use crate::{
    api::{
        content,
        handlers::{
            ContentService,
            convert::{prost_struct_to_hashmap, prost_struct_to_json_map},
        },
    },
    core::event::EventOperation,
    db::{AccessResult, LocaleContext},
    service::{self, RunnerWriteHooks, ServiceError, WriteInput},
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

        let (modified, updated_ids) =
            task::spawn_blocking(move || -> Result<(i64, Vec<String>), Status> {
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

                let user_doc = auth_user.as_ref().map(|au| &au.user_doc);

                // Phase 1: Find all matching docs and check access in one
                // read-only transaction. Unlike DeleteMany (which re-queries
                // because deleted docs leave the result set), updated docs
                // still match the same filter, so we must collect IDs upfront
                // to avoid re-updating the same docs in an infinite loop.
                let doc_ids = {
                    let tx = conn
                        .transaction_immediate()
                        .context("Start read transaction")
                        .map_err(|e| Status::from(ServiceError::classify(e, &db_kind)))?;

                    let filters = build_bulk_filters(
                        &collection,
                        &def_owned,
                        &read_access,
                        req_where.as_deref(),
                        !draft,
                    )?;

                    let find_query = crate::db::query::FindQuery::builder()
                        .filters(filters)
                        .build();

                    let docs = crate::db::query::find(
                        &tx,
                        &collection,
                        &def_owned,
                        &find_query,
                        locale_ctx.as_ref(),
                    )
                    .map_err(|e| Status::from(ServiceError::classify(e, &db_kind)))?;

                    check_per_doc_access(
                        &docs,
                        def_owned.access.update.as_deref(),
                        user_doc,
                        &hook_runner,
                        &tx,
                        "Update access denied",
                    )?;

                    tx.commit()
                        .context("Commit read transaction")
                        .map_err(|e| Status::from(ServiceError::classify(e, &db_kind)))?;

                    docs.into_iter().map(|d| d.id).collect::<Vec<_>>()
                };

                // Phase 2: Update in chunks to keep transactions short.
                const CHUNK_SIZE: usize = 500;

                let mut count = 0i64;
                let mut ids = Vec::new();

                for chunk in doc_ids.chunks(CHUNK_SIZE) {
                    let tx = conn
                        .transaction_immediate()
                        .context("Start update transaction")
                        .map_err(|e| Status::from(ServiceError::classify(e, &db_kind)))?;

                    let wh = RunnerWriteHooks::new(&hook_runner)
                        .with_hooks_enabled(run_hooks)
                        .with_conn(&tx);

                    let ctx = service::ServiceContext::collection(&collection, &def_owned)
                        .conn(&tx)
                        .write_hooks(&wh)
                        .user(user_doc)
                        .event_transport(event_transport.clone())
                        .cache(cache.clone())
                        .build();

                    for doc_id in chunk {
                        let input = WriteInput::builder(data.clone(), &join_data)
                            .locale_ctx(locale_ctx.as_ref())
                            .draft(draft)
                            .build();

                        service::update_many_single_core(&ctx, doc_id, input, &locale_config)
                            .map_err(|e| e.reclassify(&db_kind))?;

                        ids.push(doc_id.to_string());
                        count += 1;
                    }

                    tx.commit()
                        .context("Commit update transaction")
                        .map_err(|e| Status::from(ServiceError::classify(e, &db_kind)))?;
                }

                Ok((count, ids))
            })
            .await
            .inspect_err(|e| error!("Task error: {}", e))
            .map_err(|_| Status::internal("Internal error"))??;

        self.publish_bulk_mutation_events(&req.collection, &updated_ids, EventOperation::Update);

        Ok(Response::new(content::UpdateManyResponse { modified }))
    }
}
