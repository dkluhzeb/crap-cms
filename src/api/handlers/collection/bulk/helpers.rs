//! Shared helpers for bulk operations: query, access checks, event publishing.

use tonic::Status;
use tracing::error;

use crate::{
    api::handlers::collection::filter_builder::FilterBuilder,
    core::{Document, collection::CollectionDefinition},
    db::{AccessResult, DbConnection, FilterClause, FindQuery, LocaleContext, query},
    hooks::HookRunner,
    service::ServiceError,
};

/// Safety limit for bulk operations to prevent unbounded queries.
/// Bulk ops load all matching documents into memory; this caps the maximum.
pub const BULK_QUERY_LIMIT: i64 = 10_000;

/// Build filters for a bulk operation from where-clause JSON and access constraints.
pub fn build_bulk_filters(
    def: &CollectionDefinition,
    read_access: &AccessResult,
    where_json: Option<&str>,
    exclude_drafts: bool,
) -> Result<Vec<FilterClause>, Status> {
    FilterBuilder::new(&def.fields, read_access)
        .where_json(where_json)
        .draft_filter(def.has_drafts(), exclude_drafts)
        .build()
}

/// Find matching documents, enforcing read access and the bulk query limit.
pub fn find_matching_docs(
    tx: &dyn DbConnection,
    collection: &str,
    def: &CollectionDefinition,
    filters: Vec<query::FilterClause>,
    locale_ctx: Option<&LocaleContext>,
    db_kind: &str,
) -> Result<Vec<Document>, Status> {
    let find_query = FindQuery::builder()
        .filters(filters)
        .limit(BULK_QUERY_LIMIT)
        .build();

    // Internal batch lookup for bulk mutation — not a user-facing read.
    let docs = query::find(tx, collection, def, &find_query, locale_ctx)
        .map_err(|e| Status::from(ServiceError::classify(e, db_kind)))?;

    if docs.len() >= BULK_QUERY_LIMIT as usize {
        return Err(Status::resource_exhausted(format!(
            "Query matches too many documents (limit: {}). Narrow your filter.",
            BULK_QUERY_LIMIT
        )));
    }

    Ok(docs)
}

/// All-or-nothing per-document access check.
pub fn check_per_doc_access(
    docs: &[Document],
    access_ref: Option<&str>,
    user_doc: Option<&Document>,
    hook_runner: &HookRunner,
    tx: &dyn DbConnection,
    deny_msg: &str,
) -> Result<(), Status> {
    for doc in docs {
        let result = hook_runner
            .check_access(access_ref, user_doc, Some(&doc.id), None, tx)
            .map_err(|e| {
                error!("Access check error: {}", e);
                Status::internal("Internal error")
            })?;

        if matches!(result, AccessResult::Denied) {
            return Err(Status::permission_denied(deny_msg));
        }
    }

    Ok(())
}
