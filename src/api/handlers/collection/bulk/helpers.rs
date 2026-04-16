//! Shared helpers for bulk operations: query, access checks, event publishing.

use tonic::Status;
use tracing::error;

use crate::{
    api::handlers::collection::filter_builder::FilterBuilder,
    core::{Document, collection::CollectionDefinition},
    db::{AccessResult, DbConnection, FilterClause},
    hooks::HookRunner,
};

/// Build filters for a bulk operation from where-clause JSON and access constraints.
pub fn build_bulk_filters(
    slug: &str,
    def: &CollectionDefinition,
    read_access: &AccessResult,
    where_json: Option<&str>,
    exclude_drafts: bool,
) -> Result<Vec<FilterClause>, Status> {
    FilterBuilder::new(&def.fields, read_access)
        .slug(slug)
        .where_json(where_json)
        .draft_filter(def.has_drafts(), exclude_drafts)
        .build()
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
