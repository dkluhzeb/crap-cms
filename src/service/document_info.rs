//! Document information service — ref counts, back-references, missing relations.
//!
//! Thin service wrappers for consistency. All future surfaces should call these
//! instead of the query layer directly.

use serde_json::Value;

use crate::{
    config::LocaleConfig,
    core::{FieldDefinition, Registry},
    db::{
        DbConnection,
        query::{self, BackReference, MissingRelation},
    },
};

use super::ServiceError;

/// Get the incoming reference count for a document.
///
/// Returns 0 if the `_ref_count` column is NULL or the document doesn't exist.
pub fn get_ref_count(conn: &dyn DbConnection, slug: &str, id: &str) -> Result<i64, ServiceError> {
    let count = query::ref_count::get_ref_count(conn, slug, id)?.unwrap_or(0);
    Ok(count)
}

/// Find all documents that reference a given document via relationship fields.
pub fn find_back_references(
    conn: &dyn DbConnection,
    registry: &Registry,
    target_collection: &str,
    target_id: &str,
    locale_config: &LocaleConfig,
) -> Result<Vec<BackReference>, ServiceError> {
    let refs =
        query::find_back_references(conn, registry, target_collection, target_id, locale_config)?;
    Ok(refs)
}

/// Find relations in a version snapshot that no longer exist in the registry.
pub fn find_missing_relations(
    conn: &dyn DbConnection,
    registry: &Registry,
    snapshot: &Value,
    fields: &[FieldDefinition],
) -> Vec<MissingRelation> {
    query::find_missing_relations(conn, registry, snapshot, fields)
}
