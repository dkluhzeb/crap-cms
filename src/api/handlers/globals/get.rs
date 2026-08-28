//! `GetGlobal` handler — get the single document for a global definition.
//!
//! Codec over [`op::run_blocking`] with a global [`TargetRef`].

use std::sync::Arc;

use tonic::{Request, Response, Status};

use crate::{
    api::{
        content,
        handlers::{ContentService, proto::document_to_proto},
    },
    core::collection::Surface,
    db::LocaleContext,
    service::op::{self, Credentials, GetGlobal, GetGlobalArgs, Principal, TargetRef},
};

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Get the single document for a global definition.
    pub(in crate::api::handlers) async fn get_global_impl(
        &self,
        request: Request<content::GetGlobalRequest>,
    ) -> Result<Response<content::GetGlobalResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let headers = self.metadata_headers(&metadata);
        let req = request.into_inner();

        let locale_ctx =
            LocaleContext::from_locale_string(req.locale.as_deref(), &self.infra.locale_config)
                .map_err(|e| Status::invalid_argument(e.to_string()))?;

        let args = GetGlobalArgs::builder()
            .locale_ctx(locale_ctx)
            .include_drafts(req.draft.unwrap_or(false))
            .build();

        let principal = Principal::Credentials(Credentials {
            surface: Surface::Grpc,
            bearer: token,
            session_cookie: None,
            headers,
        });

        let doc = op::run_blocking::<GetGlobal>(
            Arc::clone(&self.infra),
            principal,
            TargetRef::global(req.slug.clone()),
            args,
        )
        .await
        .map_err(|e| self.core_error_status(e))?;

        Ok(Response::new(content::GetGlobalResponse {
            document: Some(document_to_proto(&doc, &req.slug)),
        }))
    }
}
