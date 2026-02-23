//! Pool-based read-only wrappers around `query::*` functions for convenience.

use anyhow::{Context, Result};

use crate::core::{CollectionDefinition, Document};
use crate::core::collection::GlobalDefinition;
use super::DbPool;
use super::query::{self, FindQuery, FilterClause};

/// Find documents (read-only, no transaction needed).
pub fn find_documents(
    pool: &DbPool,
    slug: &str,
    def: &CollectionDefinition,
    find_query: &FindQuery,
) -> Result<Vec<Document>> {
    let conn = pool.get().context("Failed to get DB connection")?;
    query::find(&conn, slug, def, find_query)
}

/// Find a single document by ID (read-only, no transaction needed).
pub fn find_document_by_id(
    pool: &DbPool,
    slug: &str,
    def: &CollectionDefinition,
    id: &str,
) -> Result<Option<Document>> {
    let conn = pool.get().context("Failed to get DB connection")?;
    query::find_by_id(&conn, slug, def, id)
}

/// Count documents (read-only, no transaction needed).
pub fn count_documents(pool: &DbPool, slug: &str, def: &CollectionDefinition, filters: &[FilterClause]) -> Result<i64> {
    let conn = pool.get().context("Failed to get DB connection")?;
    query::count(&conn, slug, def, filters)
}

/// Get a global document (read-only, no transaction needed).
pub fn get_global(pool: &DbPool, slug: &str, def: &GlobalDefinition) -> Result<Document> {
    let conn = pool.get().context("Failed to get DB connection")?;
    query::get_global(&conn, slug, def)
}
