//! Unpublish with version snapshot creation.

use anyhow::Result;

use crate::{
    core::{Document, FieldDefinition, collection::VersionsConfig},
    db::{DbConnection, query},
};

use super::snapshot::prune_versions;

/// Set a document's status to "draft", build+save a snapshot, and prune.
/// Used by both collection `persist_unpublish` and the globals unpublish handler.
pub fn unpublish_with_snapshot(
    conn: &dyn DbConnection,
    table: &str,
    parent_id: &str,
    fields: &[FieldDefinition],
    versions: Option<&VersionsConfig>,
    doc: &Document,
) -> Result<()> {
    query::set_document_status(conn, table, parent_id, "draft")?;
    let snapshot = query::build_snapshot(conn, table, fields, doc)?;
    query::create_version(conn, table, parent_id, "draft", &snapshot)?;
    prune_versions(conn, table, parent_id, versions)?;
    Ok(())
}
