//! ListVersions handler — list version history for a document.

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{content, service::ContentService},
    db::AccessResult,
};

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// List version history for a document.
    pub(in crate::api::service) async fn list_versions_impl(
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
        let access_read = def.access.read.clone();

        let versions = task::spawn_blocking(move || -> Result<_, Status> {
            let mut conn = pool.get().map_err(|e| {
                error!("ListVersions pool error: {}", e);
                Status::internal("Internal error")
            })?;

            let auth_user =
                ContentService::resolve_auth_user(token, &*token_provider, &registry, &conn)?;

            let access_result = ContentService::check_access_blocking(
                access_read.as_deref(),
                &auth_user,
                Some(&id),
                None,
                &runner,
                &mut conn,
            )?;

            if matches!(access_result, AccessResult::Denied) {
                return Err(Status::permission_denied("Read access denied"));
            }

            let (versions, _total) =
                crate::service::version_ops::list_versions(&conn, &collection, &id, limit, None)
                    .map_err(Status::from)?;
            Ok(versions)
        })
        .await
        .inspect_err(|e| error!("ListVersions task error: {}", e))
        .map_err(|_| Status::internal("Internal error"))??;

        let proto_versions: Vec<content::VersionInfo> = versions
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
        }))
    }
}
