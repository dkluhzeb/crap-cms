//! Bulk CreateMany RPC handler.

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{
        content,
        handlers::{
            ContentService,
            convert::{document_to_proto, prost_struct_to_hashmap, prost_struct_to_json_map},
        },
    },
    service::{self, CreateManyItem, CreateManyOptions, ServiceContext, ServiceError},
};

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Bulk create multiple documents. Runs per-document lifecycle hooks by default.
    pub(in crate::api::handlers) async fn create_many_impl(
        &self,
        request: Request<content::CreateManyRequest>,
    ) -> Result<Response<content::CreateManyResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let req = request.into_inner();
        let def = self.get_collection_def(&req.collection)?;

        let items: Vec<CreateManyItem> = req
            .documents
            .iter()
            .map(|s| {
                let data = prost_struct_to_hashmap(s);
                let join_data = prost_struct_to_json_map(s);

                CreateManyItem {
                    data,
                    join_data,
                    password: None,
                }
            })
            .collect();

        let run_hooks = req.hooks.unwrap_or(true);
        let draft = req.draft.unwrap_or(false);

        let pool = self.pool.clone();
        let hook_runner = self.hook_runner.clone();
        let token_provider = self.token_provider.clone();
        let registry = self.registry.clone();
        let db_kind = self.db_kind.clone();
        let collection = req.collection.clone();
        let def_owned = def;
        let event_transport = self.event_transport.clone();
        let cache = Some(self.cache.clone());

        let result = task::spawn_blocking(move || -> Result<_, Status> {
            let conn = pool
                .get()
                .map_err(|e| Status::from(ServiceError::classify(e, &db_kind)))?;

            let auth_user =
                ContentService::resolve_auth_user(token, &*token_provider, &registry, &conn)?;

            let user_doc = auth_user.as_ref().map(|au| &au.user_doc);

            let ctx = ServiceContext::collection(&collection, &def_owned)
                .pool(&pool)
                .runner(&hook_runner)
                .user(user_doc)
                .event_transport(event_transport)
                .cache(cache)
                .build();

            let opts = CreateManyOptions { run_hooks, draft };

            let result = service::create_many(&ctx, items, &opts)
                .map_err(|e| Status::from(e.reclassify(&db_kind)))?;

            let proto_docs: Vec<content::Document> = result
                .documents
                .iter()
                .map(|doc| document_to_proto(doc, &collection))
                .collect();

            Ok((result.created, proto_docs))
        })
        .await
        .inspect_err(|e| error!("Task error: {}", e))
        .map_err(|_| Status::internal("Internal error"))??;

        Ok(Response::new(content::CreateManyResponse {
            created: result.0,
            documents: result.1,
        }))
    }
}
