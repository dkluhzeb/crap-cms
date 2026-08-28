//! Create handler — create a new document in a collection.
//!
//! Codec over [`op::run_blocking`]: decode the proto request (typed
//! `DataMap` → fields, reserved password separated), dispatch, encode.

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
    service::op::{self, Create, CreateArgs, Credentials, Principal, TargetRef},
};

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Create a new document, running before/after hooks within a transaction.
    pub(in crate::api::handlers) async fn create_impl(
        &self,
        request: Request<content::CreateRequest>,
    ) -> Result<Response<content::CreateResponse>, Status> {
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

        let password = extract_auth_password(
            &mut data,
            def.is_auth_collection(),
            &self.infra.password_policy,
            false,
        )?;

        let locale_ctx =
            LocaleContext::from_locale_string(req.locale.as_deref(), &self.infra.locale_config)
                .map_err(|e| Status::invalid_argument(e.to_string()))?;

        let args = CreateArgs::builder(data)
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

        let (doc, _req_context) = op::run_blocking::<Create>(
            Arc::clone(&self.infra),
            principal,
            TargetRef::collection(req.collection.clone()),
            args,
        )
        .await
        .map_err(|e| self.core_error_status(e))?;

        Ok(Response::new(content::CreateResponse {
            document: Some(document_to_proto(&doc, &req.collection)),
        }))
    }
}
