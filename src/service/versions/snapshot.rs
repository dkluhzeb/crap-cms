//! Version snapshot context and creation/pruning helpers.

use anyhow::Result;

use crate::{
    core::{Builder, Document, FieldDefinition, collection::VersionsConfig},
    db::{DbConnection, query},
};

/// Context for creating a version snapshot, bundling the table/document metadata.
#[derive(Builder)]
pub(crate) struct VersionSnapshotCtx<'a> {
    #[builder(required)]
    pub(in crate::service::versions) table: &'a str,
    #[builder(required)]
    pub(in crate::service::versions) parent_id: &'a str,
    #[builder(default = &[])]
    pub(in crate::service::versions) fields: &'a [FieldDefinition],
    pub(in crate::service::versions) versions: Option<&'a VersionsConfig>,
    pub(in crate::service::versions) has_drafts: bool,
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
    let mut snapshot = query::build_snapshot(conn, ctx.table, ctx.fields, doc)?;

    // The snapshot must record the status this version is stamped with, not
    // whatever the in-memory doc happened to carry: on a draft create, `doc`
    // was re-read BEFORE the draft stamp above and still says "published".
    // Reads treat the row as the authority regardless, but snapshots should
    // not store a value that was never true.
    if ctx.has_drafts
        && let Some(obj) = snapshot.as_object_mut()
    {
        obj.insert(
            "_status".to_string(),
            serde_json::Value::String(status.to_string()),
        );
    }

    query::create_version(conn, ctx.table, ctx.parent_id, status, &snapshot)?;
    prune_versions(conn, ctx.table, ctx.parent_id, ctx.versions)?;
    Ok(())
}

/// Prune versions if `max_versions` is configured and > 0.
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

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use super::*;
    use crate::db::InMemoryConn;

    #[test]
    fn builder_defaults_to_empty_fields_no_config_no_drafts() {
        let ctx = VersionSnapshotCtx::builder("posts", "doc-1").build();
        assert_eq!(ctx.table, "posts");
        assert_eq!(ctx.parent_id, "doc-1");
        assert!(ctx.fields.is_empty());
        assert!(ctx.versions.is_none());
        assert!(!ctx.has_drafts);
    }

    #[test]
    fn builder_wires_each_field_to_its_own_slot() {
        let vc = VersionsConfig::new(true, 5);
        let ctx = VersionSnapshotCtx::builder("pages", "doc-9")
            .versions(Some(&vc))
            .has_drafts(true)
            .build();

        assert_eq!(ctx.table, "pages");
        assert_eq!(ctx.parent_id, "doc-9");
        assert!(ctx.has_drafts);
        assert_eq!(ctx.versions.map(|v| v.max_versions), Some(5));
    }

    /// The guard must short-circuit before touching the DB. The bare in-memory
    /// connection has no `_versions` table, so any actual prune query would
    /// error — `Ok` proves the short-circuit fired.
    #[test]
    fn prune_short_circuits_without_a_config() {
        let conn = InMemoryConn::open();
        assert!(prune_versions(&conn, "posts", "doc-1", None).is_ok());
    }

    #[test]
    fn prune_short_circuits_when_max_versions_is_zero() {
        let conn = InMemoryConn::open();
        let vc = VersionsConfig::new(true, 0);
        assert!(prune_versions(&conn, "posts", "doc-1", Some(&vc)).is_ok());
    }
}
