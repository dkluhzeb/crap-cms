//! Validate handler — check field data against collection rules without
//! persisting.
//!
//! Codec over [`op::run_blocking`]: the dry-run pipeline (field-access
//! stripping as the resolved user, field hooks, validators, unique checks)
//! lives in the shared [`Validate`] operation body.

use std::sync::Arc;

use tonic::{Request, Response, Status};

use crate::{
    api::{
        content,
        handlers::{ContentService, proto::data_map_to_json_map},
    },
    core::{DocumentFields, collection::Surface},
    db::LocaleContext,
    service::op::{self, Credentials, Principal, TargetRef, Validate, ValidateArgs},
};

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Validate document data without persisting — returns per-field errors.
    pub(in crate::api::handlers) async fn validate_impl(
        &self,
        request: Request<content::ValidateRequest>,
    ) -> Result<Response<content::ValidateResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let headers = self.metadata_headers(&metadata);
        let req = request.into_inner();

        let data: DocumentFields = req
            .data
            .map(|s| data_map_to_json_map(&s))
            .transpose()
            .map_err(Status::invalid_argument)?
            .unwrap_or_default()
            .into();

        let locale_ctx =
            LocaleContext::from_locale_string(req.locale.as_deref(), &self.infra.locale_config)
                .map_err(|e| Status::invalid_argument(e.to_string()))?;

        let args = ValidateArgs::builder(data)
            .locale_ctx(locale_ctx)
            .exclude_id(req.id.clone())
            .draft(req.draft.unwrap_or(false))
            .build();

        let principal = Principal::Credentials(Credentials {
            surface: Surface::Grpc,
            bearer: token,
            session_cookie: None,
            headers,
        });

        let outcome = op::run_blocking::<Validate>(
            Arc::clone(&self.infra),
            principal,
            TargetRef::collection(req.collection),
            args,
        )
        .await
        .map_err(|e| self.core_error_status(e))?;

        let (valid, errors) = match outcome {
            None => (true, std::collections::HashMap::new()),
            Some(ve) => (false, ve.to_field_map()),
        };

        Ok(Response::new(content::ValidateResponse { valid, errors }))
    }
}
