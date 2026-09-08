//! Unpublish with version snapshot creation.

use anyhow::Result;

use crate::{
    config::LocaleConfig,
    core::{Document, FieldDefinition, collection::VersionsConfig},
    db::{DbConnection, query},
};

use super::snapshot::prune_versions;

/// Set a document's status to "draft", build+save a snapshot, and prune.
/// Used by both collection `persist_unpublish` and the globals unpublish handler.
pub(crate) fn unpublish_with_snapshot(
    conn: &dyn DbConnection,
    table: &str,
    parent_id: &str,
    fields: &[FieldDefinition],
    versions: Option<&VersionsConfig>,
    doc: &Document,
    locale_config: Option<&LocaleConfig>,
) -> Result<()> {
    // Same `_status` guard as the restore path: a `drafts = false` collection
    // has no such column. (`unpublish` is meaningless without drafts, but the
    // capability gate is `has_versions`, so guard rather than crash.)
    if versions.is_some_and(|v| v.drafts) {
        query::set_document_status(conn, table, parent_id, "draft")?;
    }

    let snapshot = query::build_snapshot(conn, table, fields, doc, locale_config)?;

    query::create_version(conn, table, parent_id, "draft", &snapshot)?;

    prune_versions(conn, table, parent_id, versions)?;

    Ok(())
}
