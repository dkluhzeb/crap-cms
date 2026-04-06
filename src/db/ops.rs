//! Pool-based read-only wrappers around `query::*` functions for convenience.

use anyhow::{Context as _, Result};
use serde_json::Value;
use std::collections::HashMap;

use crate::core::{
    CollectionDefinition, Document, collection::GlobalDefinition, document::DocumentBuilder,
};
use crate::db::{
    DbConnection, DbPool, Filter, FilterClause, FilterOp, FindQuery, LocaleContext, query,
};

/// Find documents (read-only, no transaction needed).
pub fn find_documents(
    pool: &DbPool,
    slug: &str,
    def: &CollectionDefinition,
    find_query: &FindQuery,
    locale_ctx: Option<&LocaleContext>,
) -> Result<Vec<Document>> {
    let conn = pool.get().context("Failed to get DB connection")?;
    query::find(&conn, slug, def, find_query, locale_ctx)
}

/// Find a single document by ID (read-only, no transaction needed).
pub fn find_document_by_id(
    pool: &DbPool,
    slug: &str,
    def: &CollectionDefinition,
    id: &str,
    locale_ctx: Option<&LocaleContext>,
) -> Result<Option<Document>> {
    let conn = pool.get().context("Failed to get DB connection")?;
    query::find_by_id(&conn, slug, def, id, locale_ctx)
}

/// Count documents (read-only, no transaction needed).
pub fn count_documents(
    pool: &DbPool,
    slug: &str,
    def: &CollectionDefinition,
    filters: &[FilterClause],
    locale_ctx: Option<&LocaleContext>,
) -> Result<i64> {
    let conn = pool.get().context("Failed to get DB connection")?;
    query::count(&conn, slug, def, filters, locale_ctx)
}

/// Get a global document (read-only, no transaction needed).
pub fn get_global(
    pool: &DbPool,
    slug: &str,
    def: &GlobalDefinition,
    locale_ctx: Option<&LocaleContext>,
) -> Result<Document> {
    let conn = pool.get().context("Failed to get DB connection")?;
    query::get_global(&conn, slug, def, locale_ctx)
}

/// Find a document by ID with full hydration and optional draft overlay.
///
/// Unified read path used by admin UI, gRPC, and Lua. Handles:
/// - Draft overlay: if `use_draft` is true and the latest version is a draft,
///   returns the document from the version snapshot (blocks/arrays included).
/// - Access constraints: if `constraints` is Some, uses a filtered find instead
///   of a direct find_by_id.
/// - Hydration: join table data (blocks, arrays, has-many) is hydrated unless
///   a draft snapshot was used (snapshots already contain everything).
pub fn find_by_id_full(
    conn: &dyn DbConnection,
    slug: &str,
    def: &CollectionDefinition,
    id: &str,
    locale_ctx: Option<&LocaleContext>,
    constraints: Option<Vec<FilterClause>>,
    use_draft: bool,
) -> Result<Option<Document>> {
    // Draft snapshot check first — if the latest version is a draft, use it directly.
    // The snapshot contains all fields including blocks/arrays, so no hydration needed.
    if use_draft
        && def.has_drafts()
        && let Some(version) = query::find_latest_version(conn, slug, id)?
        && version.status == "draft"
        && let Some(doc) = document_from_snapshot(id, &version.snapshot)
    {
        return Ok(Some(doc));
    }

    // Find from main table (with or without access constraints)
    let mut doc = if let Some(constraint_filters) = constraints {
        let mut filters = constraint_filters;
        filters.push(FilterClause::Single(Filter {
            field: "id".to_string(),
            op: FilterOp::Equals(id.to_string()),
        }));
        let fq = FindQuery::builder().filters(filters).build();
        query::find(conn, slug, def, &fq, locale_ctx)?
            .into_iter()
            .next()
    } else {
        query::find_by_id_raw(conn, slug, def, id, locale_ctx, false)?
    };

    // Hydrate join table data (blocks, arrays, has-many relationships)
    if let Some(ref mut d) = doc {
        query::hydrate_document(conn, slug, &def.fields, d, None, locale_ctx)?;
    }

    Ok(doc)
}

/// Reconstruct a Document from a version snapshot JSON object.
fn document_from_snapshot(id: &str, snapshot: &Value) -> Option<Document> {
    let obj = snapshot.as_object()?;
    let mut fields: HashMap<String, Value> = obj.clone().into_iter().collect();

    let created_at = fields
        .remove("created_at")
        .and_then(|v| v.as_str().map(str::to_string));
    let updated_at = fields
        .remove("updated_at")
        .and_then(|v| v.as_str().map(str::to_string));

    Some(
        DocumentBuilder::new(id)
            .fields(fields)
            .created_at(created_at)
            .updated_at(updated_at)
            .build(),
    )
}
