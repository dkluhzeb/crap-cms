//! Version snapshot context and creation/pruning helpers.

use anyhow::Result;

use crate::{
    core::{Document, FieldDefinition, collection::VersionsConfig},
    db::{DbConnection, query},
};

/// Context for creating a version snapshot, bundling the table/document metadata.
pub(crate) struct VersionSnapshotCtx<'a> {
    pub(in crate::service::versions) table: &'a str,
    pub(in crate::service::versions) parent_id: &'a str,
    pub(in crate::service::versions) fields: &'a [FieldDefinition],
    pub(in crate::service::versions) versions: Option<&'a VersionsConfig>,
    pub(in crate::service::versions) has_drafts: bool,
}

impl<'a> VersionSnapshotCtx<'a> {
    /// Create a builder with the required table and parent_id fields.
    pub fn builder(table: &'a str, parent_id: &'a str) -> VersionSnapshotCtxBuilder<'a> {
        VersionSnapshotCtxBuilder::new(table, parent_id)
    }
}

/// Builder for [`VersionSnapshotCtx`]. Created via [`VersionSnapshotCtx::builder`].
pub(crate) struct VersionSnapshotCtxBuilder<'a> {
    table: &'a str,
    parent_id: &'a str,
    fields: &'a [FieldDefinition],
    versions: Option<&'a VersionsConfig>,
    has_drafts: bool,
}

impl<'a> VersionSnapshotCtxBuilder<'a> {
    pub(crate) fn new(table: &'a str, parent_id: &'a str) -> Self {
        Self {
            table,
            parent_id,
            fields: &[],
            versions: None,
            has_drafts: false,
        }
    }

    pub fn fields(mut self, fields: &'a [FieldDefinition]) -> Self {
        self.fields = fields;
        self
    }

    pub fn versions(mut self, versions: Option<&'a VersionsConfig>) -> Self {
        self.versions = versions;
        self
    }

    pub fn has_drafts(mut self, has_drafts: bool) -> Self {
        self.has_drafts = has_drafts;
        self
    }

    pub fn build(self) -> VersionSnapshotCtx<'a> {
        VersionSnapshotCtx {
            table: self.table,
            parent_id: self.parent_id,
            fields: self.fields,
            versions: self.versions,
            has_drafts: self.has_drafts,
        }
    }
}

/// Set document status, create a version snapshot, and prune.
pub(crate) fn create_version_snapshot(
    conn: &dyn DbConnection,
    ctx: &VersionSnapshotCtx<'_>,
    status: &str,
    doc: &Document,
) -> Result<()> {
    if ctx.has_drafts {
        query::set_document_status(conn, ctx.table, ctx.parent_id, status)?;
    }
    let snapshot = query::build_snapshot(conn, ctx.table, ctx.fields, doc)?;
    query::create_version(conn, ctx.table, ctx.parent_id, status, &snapshot)?;
    prune_versions(conn, ctx.table, ctx.parent_id, ctx.versions)?;
    Ok(())
}

/// Prune versions if max_versions is configured and > 0.
pub(crate) fn prune_versions(
    conn: &dyn DbConnection,
    table: &str,
    parent_id: &str,
    versions: Option<&VersionsConfig>,
) -> Result<()> {
    if let Some(vc) = versions
        && vc.max_versions > 0
    {
        query::prune_versions(conn, table, parent_id, vc.max_versions)?;
    }
    Ok(())
}
