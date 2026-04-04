//! Shared helpers for read handlers: field access stripping, post-processing, pagination.

use tonic::Status;

use crate::{
    api::{content, service::collection::helpers::map_db_error},
    core::{Document, upload},
    db::{BoxedConnection, LocaleContext, query},
    hooks::{HookRunner, lifecycle::AfterReadCtx},
};

/// Context for post-processing fetched documents (hydration, uploads, hooks).
pub struct PostProcessCtx<'a> {
    pub conn: &'a BoxedConnection,
    pub collection: &'a str,
    pub def: &'a crate::core::CollectionDefinition,
    pub select: Option<&'a [String]>,
    pub locale_ctx: Option<&'a LocaleContext>,
    pub runner: &'a HookRunner,
    pub hooks: &'a crate::core::collection::Hooks,
    pub fields: &'a [crate::core::FieldDefinition],
    pub user_doc: Option<&'a Document>,
    pub db_kind: &'a str,
}

/// Hydrate join data, assemble upload sizes, and apply after-read hooks for a list of docs.
pub fn post_process_docs(docs: &mut Vec<Document>, ctx: &PostProcessCtx) -> Result<(), Status> {
    for doc in docs.iter_mut() {
        query::hydrate_document(
            ctx.conn,
            ctx.collection,
            &ctx.def.fields,
            doc,
            ctx.select,
            ctx.locale_ctx,
        )
        .map_err(|e| map_db_error(e, "Query error", ctx.db_kind))?;
    }

    if let Some(ref upload_config) = ctx.def.upload
        && upload_config.enabled
    {
        for doc in docs.iter_mut() {
            upload::assemble_sizes_object(doc, upload_config);
        }
    }

    let ar_ctx = AfterReadCtx {
        hooks: ctx.hooks,
        fields: ctx.fields,
        collection: ctx.collection,
        operation: "find",
        user: ctx.user_doc,
        ui_locale: None,
    };

    *docs = ctx
        .runner
        .apply_after_read_many(&ar_ctx, std::mem::take(docs));

    Ok(())
}

/// Convert a [`query::PaginationResult`] to a gRPC `PaginationInfo` message.
pub fn pagination_result_to_proto(pr: &query::PaginationResult) -> content::PaginationInfo {
    content::PaginationInfo {
        total_docs: pr.total_docs,
        limit: pr.limit,
        total_pages: pr.total_pages,
        page: pr.page,
        page_start: pr.page_start,
        has_prev_page: pr.has_prev_page,
        has_next_page: pr.has_next_page,
        prev_page: pr.prev_page,
        next_page: pr.next_page,
        start_cursor: pr.start_cursor.clone(),
        end_cursor: pr.end_cursor.clone(),
    }
}
