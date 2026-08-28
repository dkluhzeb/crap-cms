//! `ListVersions` handler — list version history for a document.
//!
//! Codec over [`op::run_blocking`].

use std::sync::Arc;

use tonic::{Request, Response, Status};

use crate::{
    api::handlers::proto::pagination_result_to_proto,
    api::{
        content,
        handlers::{ContentService, enum_mapping},
    },
    core::collection::Surface,
    service::op::{self, Credentials, ListVersions, ListVersionsArgs, Principal, TargetRef},
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
        let headers = self.metadata_headers(&metadata);
        let req = request.into_inner();
        // Pure decode — the limit floor lives at the service chokepoint.
        let args = ListVersionsArgs::builder(req.id.clone())
            .limit(req.limit)
            .build();

        let principal = Principal::Credentials(Credentials {
            surface: Surface::Grpc,
            bearer: token,
            session_cookie: None,
            headers,
        });

        let result = op::run_blocking::<ListVersions>(
            Arc::clone(&self.infra),
            principal,
            TargetRef::collection(req.collection),
            args,
        )
        .await
        .map_err(|e| self.core_error_status(e))?;

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
