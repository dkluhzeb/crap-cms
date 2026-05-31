//! Pool-based read-only wrappers around `query::*` functions for convenience.

use anyhow::{Context as _, Result};
use serde_json::Value;

use crate::core::{
    CollectionDefinition, Document, DocumentFields, collection::GlobalDefinition,
    document::DocumentBuilder,
};
use crate::db::{
    DbConnection, DbPool, Filter, FilterClause, FilterOp, FindQuery, LocaleContext, query,
};

/// Find documents (read-only, no transaction needed).
///
/// # Errors
///
/// Returns a backend error if the connection acquisition or query fails.
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
///
/// # Errors
///
/// Returns a backend error if the connection acquisition or query fails.
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
///
/// # Errors
///
/// Returns a backend error if the connection acquisition or COUNT query fails.
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
///
/// # Errors
///
/// Returns a backend error if the connection acquisition or query fails.
pub fn get_global(
    pool: &DbPool,
    slug: &str,
    def: &GlobalDefinition,
    locale_ctx: Option<&LocaleContext>,
) -> Result<Document> {
    let conn = pool.get().context("Failed to get DB connection")?;
    query::get_global(&conn, slug, def, locale_ctx)
}

/// Parameters for [`find_by_id_full`].
pub struct FindByIdFullParams<'a> {
    pub conn: &'a dyn DbConnection,
    pub slug: &'a str,
    pub def: &'a CollectionDefinition,
    pub id: &'a str,
    pub locale_ctx: Option<&'a LocaleContext>,
    pub constraints: Option<Vec<FilterClause>>,
    pub use_draft: bool,
    pub include_deleted: bool,
}

/// Find a document by ID with full hydration and optional draft overlay.
///
/// Unified read path used by admin UI, gRPC, and Lua. Handles:
/// - Draft overlay: if `use_draft` is true and the latest version is a draft,
///   returns the document from the version snapshot (blocks/arrays included).
/// - Access constraints: if `constraints` is Some, uses a filtered find instead
///   of a direct `find_by_id`.
/// - Hydration: join table data (blocks, arrays, has-many) is hydrated unless
///   a draft snapshot was used (snapshots already contain everything).
///
/// # Errors
///
/// Returns a backend error if any of the underlying queries fails.
pub fn find_by_id_full(p: FindByIdFullParams<'_>) -> Result<Option<Document>> {
    if p.use_draft
        && p.def.has_drafts()
        && let Some(version) = query::find_latest_version(p.conn, p.slug, p.id)?
        && version.status == "draft"
        && let Some(doc) = document_from_snapshot(p.id, &version.snapshot)
    {
        return Ok(Some(doc));
    }

    let mut doc = if let Some(constraint_filters) = p.constraints {
        let mut filters = constraint_filters;
        filters.push(FilterClause::Single(Filter {
            field: "id".to_string(),
            op: FilterOp::Equals(p.id.to_string()),
        }));
        let fq = FindQuery::builder()
            .filters(filters)
            .include_deleted(p.include_deleted)
            .build();
        query::find(p.conn, p.slug, p.def, &fq, p.locale_ctx)?
            .into_iter()
            .next()
    } else {
        query::find_by_id_raw(p.conn, p.slug, p.def, p.id, p.locale_ctx, p.include_deleted)?
    };

    if let Some(ref mut d) = doc {
        query::hydrate_document(p.conn, p.slug, &p.def.fields, d, None, p.locale_ctx)?;
    }

    Ok(doc)
}

/// Reconstruct a Document from a version snapshot JSON object.
fn document_from_snapshot(id: &str, snapshot: &Value) -> Option<Document> {
    let obj = snapshot.as_object()?;
    let mut fields: DocumentFields = obj.clone().into_iter().collect();

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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn snapshot_object_lifts_timestamps_out_of_fields() {
        let snapshot = json!({
            "title": "Hello",
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-02T00:00:00Z"
        });

        let doc = document_from_snapshot("doc-1", &snapshot).unwrap();

        assert_eq!(&*doc.id, "doc-1");
        assert_eq!(doc.created_at.as_deref(), Some("2026-01-01T00:00:00Z"));
        assert_eq!(doc.updated_at.as_deref(), Some("2026-01-02T00:00:00Z"));

        // Timestamps are lifted into Document metadata, not left in `fields`.
        assert_eq!(doc.fields.get_str("title"), Some("Hello"));
        assert!(!doc.fields.contains_key("created_at"));
        assert!(!doc.fields.contains_key("updated_at"));
    }

    #[test]
    fn snapshot_without_timestamps_yields_none_metadata() {
        let doc = document_from_snapshot("doc-2", &json!({ "title": "x" })).unwrap();
        assert!(doc.created_at.is_none());
        assert!(doc.updated_at.is_none());
        assert_eq!(doc.fields.get_str("title"), Some("x"));
    }

    #[test]
    fn non_object_snapshot_is_rejected() {
        assert!(document_from_snapshot("doc-3", &json!(null)).is_none());
        assert!(document_from_snapshot("doc-3", &json!([1, 2, 3])).is_none());
        assert!(document_from_snapshot("doc-3", &json!("scalar")).is_none());
    }
}
