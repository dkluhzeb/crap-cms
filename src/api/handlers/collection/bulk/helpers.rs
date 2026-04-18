//! Shared helpers for bulk operations: query building.

use tonic::Status;

use crate::{
    api::handlers::collection::filter_builder::FilterBuilder,
    core::collection::CollectionDefinition,
    db::{AccessResult, FilterClause},
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
