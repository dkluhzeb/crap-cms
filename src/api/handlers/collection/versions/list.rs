//! `ListVersions` handler — list version history for a document.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::handlers::proto::{floor_optional_limit, pagination_result_to_proto},
    api::{
        content,
        handlers::{ContentService, enum_mapping},
    },
    core::{CollectionDefinition, document::VersionSnapshot},
    service::{
        AppInfra, ListVersionsInput, PaginatedResult, RunnerReadHooks, ServiceContext,
        list_versions,
    },
};

/// Owned bundle for the `ListVersions` spawn-blocking body. Process-stable
/// dependencies come from the shared [`AppInfra`]; the rest is per-call.
struct ListVersionsBlockingInput {
    infra: Arc<AppInfra>,
    headers: HashMap<String, String>,
    collection: String,
    id: String,
    limit: Option<i64>,
    def: CollectionDefinition,
    token: Option<String>,
}

fn list_versions_blocking(
    input: &ListVersionsBlockingInput,
) -> Result<PaginatedResult<VersionSnapshot>, Status> {
    let infra = &input.infra;

    let conn = infra
        .pool
        .get()
        .inspect_err(|e| error!("ListVersions pool error: {}", e))
        .map_err(|_| Status::internal("Internal error"))?;

    let auth_user = ContentService::resolve_auth_user(
        input.token.as_deref(),
        &input.headers,
        &*infra.token_provider,
        &infra.hook_runner,
        &infra.registry,
        &conn,
    )?;

    let user_doc = auth_user.as_ref().map(|au| &au.user_doc);
    let hooks = RunnerReadHooks::new(&infra.hook_runner, &conn, user_doc, None);

    let ctx = ServiceContext::collection(&input.collection, &input.def)
        .infra(infra)
        .conn(&conn)
        .read_hooks(&hooks)
        .user(user_doc)
        .build();

    let list_input = ListVersionsInput::builder(&input.id)
        .limit(input.limit)
        .build();

    list_versions(&ctx, &list_input).map_err(Status::from)
}

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// List version history for a document.
    pub(in crate::api::handlers) async fn list_versions_impl(
        &self,
        request: Request<content::ListVersionsRequest>,
    ) -> Result<Response<content::ListVersionsResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let headers = self.metadata_headers(&metadata);
        let req = request.into_inner();
        let def = self.get_collection_def(&req.collection)?;

        if !def.has_versions() {
            return Err(Status::failed_precondition(format!(
                "Collection '{}' does not have versioning enabled",
                req.collection
            )));
        }

        let input = ListVersionsBlockingInput {
            infra: Arc::clone(&self.infra),
            collection: req.collection.clone(),
            id: req.id.clone(),
            limit: floor_optional_limit(req.limit),
            def,
            token,
            headers,
        };

        let result = task::spawn_blocking(move || list_versions_blocking(&input))
            .await
            .inspect_err(|e| error!("ListVersions task error: {}", e))
            .map_err(|_| Status::internal("Internal error"))??;

        let proto_versions: Vec<content::VersionInfo> = result
            .docs
            .iter()
            .map(|v| content::VersionInfo {
                id: v.id.clone(),
                version: v.version,
                status: enum_mapping::version_status(&v.status).into(),
                latest: v.latest,
                created_at: v.created_at.clone().unwrap_or_default(),
            })
            .collect();

        Ok(Response::new(content::ListVersionsResponse {
            versions: proto_versions,
            pagination: Some(pagination_result_to_proto(&result.pagination)),
        }))
    }
}
