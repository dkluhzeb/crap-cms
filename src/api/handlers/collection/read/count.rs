//! Count handler — count documents matching filters.
//!
//! Codec over [`op::run_blocking`]: decode the proto request (filters via
//! [`decode_where_json`] into the canonical grammar), dispatch, encode.

use std::sync::Arc;

use tonic::{Request, Response, Status};

use crate::{
    api::{
        content,
        handlers::{ContentService, collection::filter_builder::decode_where_json},
    },
    core::collection::Surface,
    db::LocaleContext,
    service::op::{self, Count, CountArgs, Credentials, Principal, TargetRef},
};

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Count documents matching filters (no per-document hooks).
    pub(in crate::api::handlers) async fn count_impl(
        &self,
        request: Request<content::CountRequest>,
    ) -> Result<Response<content::CountResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let headers = self.metadata_headers(&metadata);
        let req = request.into_inner();

        let locale_ctx =
            LocaleContext::from_locale_string(req.locale.as_deref(), &self.infra.locale_config)
                .map_err(|e| Status::invalid_argument(e.to_string()))?;

        // Pure wire decode — filter hygiene (system columns, dot paths)
        // lives in the shared `Count` body.
        let filters = decode_where_json(req.r#where.as_deref())?;

        let args = CountArgs::builder(filters)
            .locale_ctx(locale_ctx)
            .search(req.search.clone())
            .include_drafts(req.draft.unwrap_or(false))
            .trash(req.trash.unwrap_or(false))
            .build();

        let principal = Principal::Credentials(Credentials {
            surface: Surface::Grpc,
            bearer: token,
            session_cookie: None,
            headers,
        });

        let count = op::run_blocking::<Count>(
            Arc::clone(&self.infra),
            principal,
            TargetRef::collection(req.collection),
            args,
        )
        .await
        .map_err(|e| self.core_error_status(e))?;

        Ok(Response::new(content::CountResponse { count }))
    }
}
