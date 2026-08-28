//! `FindByID` handler — fetch a single document by ID.
//!
//! Stage-2 reference codec: decode the proto request into
//! [`FindByIdArgs`] + [`Principal`] + [`TargetRef`], dispatch through
//! [`op::run_blocking`], encode the result. Auth resolution, target lookup,
//! context assembly, and the definition-dependent flag downgrades live in the
//! operation core, not here.

use std::sync::Arc;

use tonic::{Request, Response, Status};

use crate::{
    api::{
        content,
        handlers::{ContentService, proto::document_to_proto},
    },
    core::collection::Surface,
    db::{LocaleContext, query},
    service::op::{self, Credentials, FindById, FindByIdArgs, Principal, TargetRef},
};

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Find a single document by ID with optional relationship population depth.
    pub(in crate::api::handlers) async fn find_by_id_impl(
        &self,
        request: Request<content::FindByIdRequest>,
    ) -> Result<Response<content::FindByIdResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let headers = self.metadata_headers(&metadata);
        let req = request.into_inner();

        let depth = query::clamp_depth(req.depth, self.default_depth, self.max_depth);

        let select = if req.select.is_empty() {
            None
        } else {
            Some(req.select.clone())
        };

        let locale_ctx =
            LocaleContext::from_locale_string(req.locale.as_deref(), &self.infra.locale_config)
                .map_err(|e| Status::invalid_argument(e.to_string()))?;

        let args = FindByIdArgs::builder(req.id.clone())
            .depth(depth)
            .select(select)
            .locale_ctx(locale_ctx)
            .use_draft(req.draft.unwrap_or(false))
            .include_deleted(req.trash.unwrap_or(false))
            .build();

        let principal = Principal::Credentials(Credentials {
            surface: Surface::Grpc,
            bearer: token,
            session_cookie: None,
            headers,
        });

        let doc = op::run_blocking::<FindById>(
            Arc::clone(&self.infra),
            principal,
            TargetRef::collection(req.collection.clone()),
            args,
        )
        .await
        .map_err(|e| self.core_error_status(e))?;

        match doc {
            Some(d) => Ok(Response::new(content::FindByIdResponse {
                document: Some(document_to_proto(&d, &req.collection)),
            })),
            None => Err(Status::not_found(format!(
                "Document '{}' not found in '{}'",
                req.id, req.collection
            ))),
        }
    }
}
