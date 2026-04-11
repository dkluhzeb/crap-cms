//! ListVersions handler — list version history for a document.

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::handlers::convert::pagination_result_to_proto,
    api::{content, handlers::ContentService},
    service::{ListVersionsInput, RunnerReadHooks, ServiceContext, list_versions},
};

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// List version history for a document.
    pub(in crate::api::handlers) async fn list_versions_impl(
        &self,
        request: Request<content::ListVersionsRequest>,
    ) -> Result<Response<content::ListVersionsResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let req = request.into_inner();
        let def = self.get_collection_def(&req.collection)?;

        if !def.has_versions() {
            return Err(Status::failed_precondition(format!(
                "Collection '{}' does not have versioning enabled",
                req.collection
            )));
        }

        let pool = self.pool.clone();
        let runner = self.hook_runner.clone();
        let token_provider = self.token_provider.clone();
        let registry = self.registry.clone();
        let collection = req.collection.clone();
        let id = req.id.clone();
        let limit = req.limit;

        let result = task::spawn_blocking(move || -> Result<_, Status> {
            let conn = pool.get().map_err(|e| {
                error!("ListVersions pool error: {}", e);
                Status::internal("Internal error")
            })?;

            let auth_user =
                ContentService::resolve_auth_user(token, &*token_provider, &registry, &conn)?;

            let user_doc = auth_user.as_ref().map(|au| &au.user_doc);
            let hooks = RunnerReadHooks::new(&runner, &conn);

            let ctx = ServiceContext::collection(&collection, &def)
                .conn(&conn)
                .read_hooks(&hooks)
                .user(user_doc)
                .build();

            let input = ListVersionsInput::builder(&id).limit(limit).build();

            let result = list_versions(&ctx, &input).map_err(Status::from)?;

            Ok(result)
        })
        .await
        .inspect_err(|e| error!("ListVersions task error: {}", e))
        .map_err(|_| Status::internal("Internal error"))??;

        let proto_versions: Vec<content::VersionInfo> = result
            .docs
            .iter()
            .map(|v| content::VersionInfo {
                id: v.id.clone(),
                version: v.version,
                status: v.status.clone(),
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
