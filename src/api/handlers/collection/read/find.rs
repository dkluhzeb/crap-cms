//! Find handler — list documents with filters, sorting, and pagination.
//!
//! Codec over [`op::run_blocking`]: decode the proto request into the
//! canonical [`FindQuery`] + [`FindArgs`], dispatch, encode. The trash
//! downgrade, trash default order, and query-field validation live in the
//! operation body.

use std::sync::Arc;

use tonic::{Request, Response, Status};

use crate::{
    api::{
        content,
        handlers::{
            ContentService,
            collection::filter_builder::decode_where_json,
            proto::{document_to_proto, pagination_result_to_proto},
        },
    },
    core::collection::Surface,
    db::{FindQuery, LocaleContext, query},
    service::op::{self, Credentials, Find, FindArgs, Principal, TargetRef},
};

/// Build a `FindQuery` from the gRPC request parameters.
///
/// Produces a *user* query — system filters (`_status`, `_deleted_at`) are
/// injected by the service layer based on the typed `trash` /
/// `include_drafts` flags.
fn build_find_query(
    req: &content::FindRequest,
    pagination: &query::FindPagination,
    select: Option<&[String]>,
) -> Result<FindQuery, Status> {
    let filters = decode_where_json(req.r#where.as_deref())?;

    let offset = (!pagination.has_cursor()).then_some(pagination.offset);

    Ok(FindQuery::builder()
        .filters(filters)
        .order_by(req.order_by.clone())
        .limit(Some(pagination.limit))
        .offset(offset)
        .select(select.map(<[String]>::to_vec))
        .after_cursor(pagination.after_cursor.clone())
        .before_cursor(pagination.before_cursor.clone())
        .search(req.search.clone())
        .build())
}

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Find documents in a collection with optional filters, sorting, and pagination.
    pub(in crate::api::handlers) async fn find_impl(
        &self,
        request: Request<content::FindRequest>,
    ) -> Result<Response<content::FindResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let headers = self.metadata_headers(&metadata);
        let req = request.into_inner();

        let select = if req.select.is_empty() {
            None
        } else {
            Some(req.select.clone())
        };

        let pagination = self
            .pagination_ctx
            .validate(
                req.limit,
                req.page,
                req.after_cursor.as_deref(),
                req.before_cursor.as_deref(),
            )
            .map_err(Status::invalid_argument)?;

        let locale_ctx =
            LocaleContext::from_locale_string(req.locale.as_deref(), &self.infra.locale_config)
                .map_err(|e| Status::invalid_argument(e.to_string()))?;
        let depth = query::clamp_depth(req.depth, self.default_depth, self.max_depth);

        let find_query = build_find_query(&req, &pagination, select.as_deref())?;

        let args = FindArgs::builder(find_query)
            .depth(depth)
            .locale_ctx(locale_ctx)
            .cursor_enabled(self.pagination_ctx.cursor_enabled)
            .trash(req.trash.unwrap_or(false))
            .include_drafts(req.draft.unwrap_or(false))
            .build();

        let principal = Principal::Credentials(Credentials {
            surface: Surface::Grpc,
            bearer: token,
            session_cookie: None,
            headers,
        });

        let result = op::run_blocking::<Find>(
            Arc::clone(&self.infra),
            principal,
            TargetRef::collection(req.collection.clone()),
            args,
        )
        .await
        .map_err(|e| self.core_error_status(e))?;

        let proto_docs: Vec<_> = result
            .docs
            .iter()
            .map(|doc| document_to_proto(doc, &req.collection))
            .collect();

        Ok(Response::new(content::FindResponse {
            documents: proto_docs,
            pagination: Some(pagination_result_to_proto(&result.pagination)),
        }))
    }
}
