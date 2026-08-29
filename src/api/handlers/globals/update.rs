//! `UpdateGlobal` handler — update the single document for a global.
//!
//! Codec over [`op::run_blocking`] with a global [`TargetRef`].

use std::sync::Arc;

use tonic::{Request, Response, Status};

use crate::{
    api::{
        content,
        handlers::{
            ContentService,
            proto::{data_map_to_json_map, document_to_proto},
        },
    },
    core::{DocumentFields, collection::Surface},
    db::LocaleContext,
    service::op::{self, Credentials, Principal, TargetRef, UpdateGlobal, UpdateGlobalArgs},
};

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Update a global's document, running hooks within a transaction.
    pub(in crate::api::handlers) async fn update_global_impl(
        &self,
        request: Request<content::UpdateGlobalRequest>,
    ) -> Result<Response<content::UpdateGlobalResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let headers = self.metadata_headers(&metadata);
        let req = request.into_inner();

        let data: DocumentFields = req
            .data
            .map(|s| data_map_to_json_map(&s))
            .unwrap_or_default()
            .into();

        let locale_ctx =
            LocaleContext::from_locale_string(req.locale.as_deref(), &self.infra.locale_config)
                .map_err(|e| Status::invalid_argument(e.to_string()))?;

        let args = UpdateGlobalArgs::builder(data)
            .locale_ctx(locale_ctx)
            .events(req.events.unwrap_or(true))
            .draft(req.draft.unwrap_or(false))
            .build();

        let principal = Principal::Credentials(Credentials {
            surface: Surface::Grpc,
            bearer: token,
            session_cookie: None,
            headers,
        });

        let (doc, _req_context) = op::run_blocking::<UpdateGlobal>(
            Arc::clone(&self.infra),
            principal,
            TargetRef::global(req.slug.clone()),
            args,
        )
        .await
        .map_err(|e| self.core_error_status(e))?;

        Ok(Response::new(content::UpdateGlobalResponse {
            document: Some(document_to_proto(&doc, &req.slug)),
        }))
    }
}
