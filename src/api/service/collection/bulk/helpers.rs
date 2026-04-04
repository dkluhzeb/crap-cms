//! Shared helpers for bulk operations: query, access checks, event publishing.

use tonic::Status;
use tracing::error;

pub(in crate::api::service::collection) use crate::api::service::collection::helpers::map_db_error;

use crate::{
    api::service::ContentService,
    core::{
        Document,
        collection::CollectionDefinition,
        event::{EventOperation, EventTarget},
    },
    db::{AccessResult, DbConnection, FindQuery, LocaleContext, query},
    hooks::{HookRunner, lifecycle::PublishEventInput},
};

/// Safety limit for bulk operations to prevent unbounded queries.
/// Bulk ops load all matching documents into memory; this caps the maximum.
pub const BULK_QUERY_LIMIT: i64 = 10_000;

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

    let docs = query::find(tx, collection, def, &find_query, locale_ctx)
        .map_err(|e| map_db_error(e, "Bulk query error", db_kind))?;

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

/// Publish mutation events for a list of document IDs.
pub fn publish_bulk_events(
    service: &ContentService,
    collection: &str,
    doc_ids: &[String],
    operation: EventOperation,
) {
    if let Ok(def) = service.get_collection_def(collection) {
        for doc_id in doc_ids {
            service.hook_runner.publish_event(
                &service.event_bus,
                &def.hooks,
                def.live.as_ref(),
                PublishEventInput::builder(EventTarget::Collection, operation.clone())
                    .collection(collection.to_string())
                    .document_id(doc_id.clone())
                    .build(),
            );
        }
    }
}
