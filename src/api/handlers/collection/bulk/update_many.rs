//! Bulk `UpdateMany` RPC handler.
//!
//! Codec over [`op::run_blocking`]. A `password` is rejected at decode — a
//! broadcast write must not set one credential on many rows.

use std::sync::Arc;

use tonic::{Request, Response, Status};

use crate::{
    api::{
        content,
        handlers::{
            ContentService, collection::filter_builder::decode_where_json,
            proto::data_map_to_json_map,
        },
    },
    core::{DocumentFields, collection::Surface},
    db::LocaleContext,
    service::op::{self, Credentials, Principal, TargetRef, UpdateMany, UpdateManyArgs},
};

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Bulk update matching documents. Runs per-document lifecycle hooks by default.
    pub(in crate::api::handlers) async fn update_many_impl(
        &self,
        request: Request<content::UpdateManyRequest>,
    ) -> Result<Response<content::UpdateManyResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let headers = self.metadata_headers(&metadata);
        let req = request.into_inner();
        let def = self.get_collection_def(&req.collection)?;

        let mut data: DocumentFields = req
            .data
            .map(|s| data_map_to_json_map(&s))
            .unwrap_or_default()
            .into();

        if def.is_auth_collection() && data.contains_key("password") {
            return Err(Status::invalid_argument(
                "Password updates are not supported in UpdateMany. Use Update for individual documents.",
            ));
        }

        if def.is_auth_collection() {
            data.remove("password");
        }

        let locale_ctx =
            LocaleContext::from_locale_string(req.locale.as_deref(), &self.infra.locale_config)
                .map_err(|e| Status::invalid_argument(e.to_string()))?;

        let filters = decode_where_json(req.r#where.as_deref())?;

        let args = UpdateManyArgs::builder(filters, data)
            .locale_ctx(locale_ctx)
            .run_hooks(req.hooks.unwrap_or(true))
            .draft(req.draft.unwrap_or(false))
            .max_documents(self.server_config.bulk_max_documents)
            .events(req.events.unwrap_or(false))
            .build();

        let principal = Principal::Credentials(Credentials {
            surface: Surface::Grpc,
            bearer: token,
            session_cookie: None,
            headers,
        });

        let result = op::run_blocking::<UpdateMany>(
            Arc::clone(&self.infra),
            principal,
            TargetRef::collection(req.collection),
            args,
        )
        .await
        .map_err(|e| self.core_error_status(e))?;

        Ok(Response::new(content::UpdateManyResponse {
            modified: result.modified,
        }))
    }
}
