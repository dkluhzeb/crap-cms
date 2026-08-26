//! Bulk `CreateMany` RPC handler.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{
        content,
        handlers::{
            ContentService,
            proto::{data_map_to_json_map, document_to_proto},
        },
    },
    core::CollectionDefinition,
    service::{self, AppInfra, CreateManyItem, CreateManyOptions, ServiceContext, ServiceError},
};

/// Owned bundle for the `CreateMany` spawn-blocking body. Process-stable
/// dependencies come from the shared [`AppInfra`]; the rest is per-call.
struct CreateManyBlockingInput {
    infra: Arc<AppInfra>,
    headers: HashMap<String, String>,
    db_kind: String,
    collection: String,
    def: CollectionDefinition,
    token: Option<String>,
    items: Vec<CreateManyItem>,
    run_hooks: bool,
    draft: bool,
    bulk_max_documents: i64,
    events: bool,
}

fn create_many_blocking(
    input: &CreateManyBlockingInput,
) -> Result<(i64, Vec<content::Document>), Status> {
    let infra = &input.infra;

    let conn = infra
        .pool
        .get()
        .map_err(|e| Status::from(ServiceError::classify(e, &input.db_kind)))?;

    let auth_user = ContentService::resolve_auth_user(
        input.token.as_deref(),
        &input.headers,
        &*infra.token_provider,
        &infra.hook_runner,
        &infra.registry,
        &conn,
    )?;

    let user_doc = auth_user.as_ref().map(|au| &au.user_doc);

    let ctx = ServiceContext::collection(&input.collection, &input.def)
        .infra(infra)
        .user(user_doc)
        .emit_events(input.events)
        .build();

    let opts = CreateManyOptions {
        run_hooks: input.run_hooks,
        draft: input.draft,
        max_documents: input.bulk_max_documents,
    };

    let result = service::create_many(&ctx, &input.items, &opts)
        .map_err(|e| Status::from(e.reclassify(&input.db_kind)))?;

    let proto_docs: Vec<content::Document> = result
        .documents
        .iter()
        .map(|doc| document_to_proto(doc, &input.collection))
        .collect();

    Ok((result.created, proto_docs))
}

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Bulk create multiple documents. Runs per-document lifecycle hooks by default.
    pub(in crate::api::handlers) async fn create_many_impl(
        &self,
        request: Request<content::CreateManyRequest>,
    ) -> Result<Response<content::CreateManyResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let headers = self.metadata_headers(&metadata);
        let req = request.into_inner();
        let def = self.get_collection_def(&req.collection)?;

        // Auth collections: split each item's `password` off so the service
        // create chokepoint validates it against the password policy and hashes
        // it — parity with single Create and Lua `create_many`. Bulk create is
        // per-item (distinct passwords per user), so seeding auth users with
        // policed passwords in one transaction is a legitimate operation; only
        // `update_many` (a broadcast that would set one password on many rows)
        // rejects a password. A non-auth collection keeps a legitimate
        // `password` field as ordinary data.
        let is_auth = def.is_auth_collection();
        let mut items: Vec<CreateManyItem> = Vec::with_capacity(req.documents.len());
        for s in &req.documents {
            let mut map = data_map_to_json_map(s);

            let password = if is_auth {
                map.remove("password")
                    .and_then(|v| v.as_str().map(std::string::ToString::to_string))
            } else {
                None
            };

            items.push(CreateManyItem {
                data: map.into(),
                password,
            });
        }

        let input = CreateManyBlockingInput {
            infra: Arc::clone(&self.infra),
            db_kind: self.db_kind.clone(),
            collection: req.collection.clone(),
            def,
            token,
            headers,
            items,
            run_hooks: req.hooks.unwrap_or(true),
            draft: req.draft.unwrap_or(false),
            bulk_max_documents: self.server_config.bulk_max_documents,
            events: req.events.unwrap_or(false),
        };

        let result = task::spawn_blocking(move || create_many_blocking(&input))
            .await
            .inspect_err(|e| error!("Task error: {}", e))
            .map_err(|_| Status::internal("Internal error"))??;

        Ok(Response::new(content::CreateManyResponse {
            created: result.0,
            documents: result.1,
        }))
    }
}
