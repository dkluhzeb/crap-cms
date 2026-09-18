//! Version snapshot context and creation helpers.

use anyhow::Result;
use serde_json::Value;

use crate::{
    config::LocaleConfig,
    core::{Builder, Document, FieldDefinition, collection::VersionsConfig},
    db::{DbConnection, query, query::VersionWrite},
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
    /// Needed so the snapshot records EVERY locale's column, not just the
    /// one the write resolved (see `build_snapshot`).
    pub(in crate::service::versions) locale_config: Option<&'a LocaleConfig>,
}

/// Set document status, create a version snapshot, and prune.
///
/// # Errors
///
/// Returns a backend error if the status update, the snapshot build or the
/// version write fails.
pub(crate) fn create_version_snapshot(
    conn: &dyn DbConnection,
    ctx: &VersionSnapshotCtx<'_>,
    status: &str,
    doc: &Document,
) -> Result<()> {
    if ctx.has_drafts {
        query::set_document_status(conn, ctx.table, ctx.parent_id, status)?;
    }

    // A format converted on the background queue is not part of this write, so
    // the row — and therefore this snapshot — still carries the PREVIOUS file's
    // derivative url in that column until the job runs and replaces it. Under
    // reference-checked deletion those bytes stay alive for as long as this
    // snapshot names them, and go when it is pruned. Recording the url that was
    // true at snapshot time is what makes a restore of this version find a file
    // it can serve, so the stale column is kept deliberately.
    let mut snapshot = query::build_snapshot(conn, ctx.table, ctx.fields, doc, ctx.locale_config)?;

    // The snapshot must record the status this version is stamped with, not
    // whatever the in-memory doc happened to carry: on a draft create, `doc`
    // was re-read BEFORE the draft stamp above and still says "published".
    // Reads treat the row as the authority regardless, but snapshots should
    // not store a value that was never true.
    if ctx.has_drafts
        && let Some(obj) = snapshot.as_object_mut()
    {
        obj.insert("_status".to_string(), Value::String(status.to_string()));
    }

    query::create_version_and_prune(
        conn,
        &VersionWrite::builder(ctx.table, ctx.parent_id, status, &snapshot)
            .max_versions(VersionsConfig::cap(ctx.versions))
            .build(),
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
