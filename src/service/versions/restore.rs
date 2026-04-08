//! Version restore operations for collections and globals.

use crate::{
    config::LocaleConfig,
    core::{CollectionDefinition, Document, collection::GlobalDefinition},
    db::{DbConnection, query, query::helpers::global_table},
    service::ServiceError,
};

/// Restore a collection document to a specific version snapshot.
///
/// Finds the version, applies the snapshot to the document, adjusts ref counts,
/// and creates a new version record. Caller manages the transaction.
pub fn restore_collection_version(
    conn: &dyn DbConnection,
    slug: &str,
    def: &CollectionDefinition,
    document_id: &str,
    version_id: &str,
    locale_config: &LocaleConfig,
) -> Result<Document, ServiceError> {
    let version = query::find_version_by_id(conn, slug, version_id)?
        .ok_or_else(|| ServiceError::NotFound(format!("Version '{version_id}' not found")))?;

    let doc = query::restore_version(
        conn,
        slug,
        def,
        document_id,
        &version.snapshot,
        "published",
        locale_config,
    )?;

    Ok(doc)
}

/// Restore a global document to a specific version snapshot.
///
/// Finds the version (using the global table name), applies the snapshot,
/// adjusts ref counts, and creates a new version record. Caller manages the transaction.
pub fn restore_global_version(
    conn: &dyn DbConnection,
    slug: &str,
    def: &GlobalDefinition,
    version_id: &str,
    locale_config: &LocaleConfig,
) -> Result<Document, ServiceError> {
    let gtable = global_table(slug);

    let version = query::find_version_by_id(conn, &gtable, version_id)?
        .ok_or_else(|| ServiceError::NotFound(format!("Version '{version_id}' not found")))?;

    let doc = query::restore_global_version(
        conn,
        slug,
        def,
        &version.snapshot,
        "published",
        locale_config,
    )?;

    Ok(doc)
}
