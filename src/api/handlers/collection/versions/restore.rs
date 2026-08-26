//! `RestoreVersion` handler — restore a document to a previous version.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{
        content,
        handlers::{ContentService, proto::document_to_proto},
    },
    core::CollectionDefinition,
    service::{AppInfra, ServiceContext, restore_collection_version},
};

/// Owned bundle for the `RestoreVersion` spawn-blocking body. Process-stable
/// dependencies come from the shared [`AppInfra`]; the rest is per-call.
struct RestoreVersionBlockingInput {
    infra: Arc<AppInfra>,
    headers: HashMap<String, String>,
    collection: String,
    document_id: String,
    version_id: String,
    def: CollectionDefinition,
    token: Option<String>,
}

fn restore_version_blocking(
    input: &RestoreVersionBlockingInput,
) -> Result<content::Document, Status> {
    let infra = &input.infra;

    let conn = infra
        .pool
        .get()
        .inspect_err(|e| error!("RestoreVersion pool error: {}", e))
        .map_err(|_| Status::internal("Internal error"))?;

    let auth_user = ContentService::resolve_auth_user(
        input.token.as_deref(),
        &input.headers,
        &*infra.token_provider,
        &infra.hook_runner,
        &infra.registry,
        &conn,
    )?;
    let user_doc = auth_user.as_ref().map(|au| au.user_doc.clone());

    let ctx = ServiceContext::collection(&input.collection, &input.def)
        .infra(infra)
        .user(user_doc.as_ref())
        .build();

    let doc = restore_collection_version(
        &ctx,
        &input.document_id,
        &input.version_id,
        &infra.locale_config,
    )
    .map_err(Status::from)?;

    Ok(document_to_proto(&doc, &input.collection))
}

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Restore a document to a previous version.
    pub(in crate::api::handlers) async fn restore_version_impl(
        &self,
        request: Request<content::RestoreVersionRequest>,
    ) -> Result<Response<content::RestoreVersionResponse>, Status> {
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

        let input = RestoreVersionBlockingInput {
            infra: Arc::clone(&self.infra),
            collection: req.collection.clone(),
            document_id: req.document_id.clone(),
            version_id: req.version_id.clone(),
            def,
            token,
            headers,
        };

        let doc = task::spawn_blocking(move || restore_version_blocking(&input))
            .await
            .inspect_err(|e| error!("RestoreVersion task error: {}", e))
            .map_err(|_| Status::internal("Internal error"))??;

        Ok(Response::new(content::RestoreVersionResponse {
            document: Some(doc),
        }))
    }
}
