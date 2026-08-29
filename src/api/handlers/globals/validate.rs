//! `ValidateGlobal` handler — check global field data against its rules
//! without persisting.
//!
//! Codec over [`op::run_blocking`]: the dry-run targets the `_global_<slug>`
//! table in update mode against the singleton `default` row — semantics live
//! in the shared [`ValidateGlobal`] operation body.

use std::sync::Arc;

use tonic::{Request, Response, Status};

use crate::{
    api::{
        content,
        handlers::{ContentService, proto::data_map_to_json_map},
    },
    core::{DocumentFields, collection::Surface},
    db::LocaleContext,
    service::op::{self, Credentials, Principal, TargetRef, ValidateArgs, ValidateGlobal},
};

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Validate global document data without persisting — returns per-field errors.
    pub(in crate::api::handlers) async fn validate_global_impl(
        &self,
        request: Request<content::ValidateGlobalRequest>,
    ) -> Result<Response<content::ValidateResponse>, Status> {
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

        let args = ValidateArgs::builder(data)
            .locale_ctx(locale_ctx)
            .draft(req.draft.unwrap_or(false))
            .build();

        let principal = Principal::Credentials(Credentials {
            surface: Surface::Grpc,
            bearer: token,
            session_cookie: None,
            headers,
        });

        let outcome = op::run_blocking::<ValidateGlobal>(
            Arc::clone(&self.infra),
            principal,
            TargetRef::global(req.slug),
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
