//! Update handler — update an existing document by ID.
//!
//! Codec over [`op::run_blocking`]. An `unpublish` request routes to the
//! unpublish handler; the shared service gate rejects unpublish on a
//! non-versioned collection with an explicit error on every surface.

use std::sync::Arc;

use tonic::{Request, Response, Status};

use crate::{
    api::{
        content,
        handlers::{
            ContentService,
            collection::helpers::extract_auth_password,
            proto::{data_map_to_json_map, document_to_proto},
        },
    },
    core::{DocumentFields, collection::Surface},
    db::LocaleContext,
    service::op::{self, Credentials, Principal, TargetRef, Update, UpdateArgs},
};

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Update an existing document by ID, running before/after hooks within a transaction.
    pub(in crate::api::handlers) async fn update_impl(
        &self,
        request: Request<content::UpdateRequest>,
    ) -> Result<Response<content::UpdateResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let headers = self.metadata_headers(&metadata);
        let req = request.into_inner();
        let def = self.get_collection_def(&req.collection)?;

        // Route an unpublish request to the unpublish path regardless of
        // versioning: the shared service gate rejects unpublish on a
        // non-versioned collection (an explicit error), instead of silently
        // falling through to a normal update.
        if req.unpublish.unwrap_or(false) {
            return self.unpublish_impl(token, headers, &req).await;
        }

        let mut data: DocumentFields = req
            .data
            .map(|s| data_map_to_json_map(&s))
            .unwrap_or_default()
            .into();

        let password = extract_auth_password(
            &mut data,
            def.is_auth_collection(),
            &self.infra.password_policy,
            true,
        )?;

        let locale_ctx =
            LocaleContext::from_locale_string(req.locale.as_deref(), &self.infra.locale_config)
                .map_err(|e| Status::invalid_argument(e.to_string()))?;

        let args = UpdateArgs::builder(req.id.clone(), data)
            .password(password)
            .locale_ctx(locale_ctx)
            .draft(req.draft.unwrap_or(false))
            .events(req.events.unwrap_or(true))
            .build();

        let principal = Principal::Credentials(Credentials {
            surface: Surface::Grpc,
            bearer: token,
            session_cookie: None,
            headers,
        });

        let (doc, _req_context) = op::run_blocking::<Update>(
            Arc::clone(&self.infra),
            principal,
            TargetRef::collection(req.collection.clone()),
            args,
        )
        .await
        .map_err(|e| self.core_error_status(e))?;

        Ok(Response::new(content::UpdateResponse {
            document: Some(document_to_proto(&doc, &req.collection)),
        }))
    }
}
