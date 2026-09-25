//! Version snapshot context and creation helpers.

use anyhow::Result;
use serde_json::Value;

use crate::{
    config::LocaleConfig,
    core::{
        Builder, CollectionDefinition, Document, FieldDefinition, GlobalDefinition,
        collection::VersionsConfig,
    },
    db::{
        DbConnection, query,
        query::{StatusTable, VersionWrite},
    },
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
    /// Whether the main table keeps `created_at`/`updated_at`. The status
    /// write bumps `updated_at` only when it does.
    #[builder(default = true)]
    pub(in crate::service::versions) timestamps: bool,
    /// Needed so the snapshot records EVERY locale's column, not just the
    /// one the write resolved (see `build_snapshot`).
    pub(in crate::service::versions) locale_config: Option<&'a LocaleConfig>,
}

impl<'a> VersionSnapshotCtx<'a> {
    /// The snapshot context of a collection document: every flag comes from
    /// the definition, so no write path can pair a status write with the wrong
    /// drafts/timestamps shape.
    pub(crate) fn for_collection(
        slug: &'a str,
        id: &'a str,
        def: &'a CollectionDefinition,
        locale_config: Option<&'a LocaleConfig>,
    ) -> Self {
        Self::builder(slug, id)
            .fields(&def.fields)
            .versions(def.versions.as_ref())
            .has_drafts(def.has_drafts())
            .timestamps(def.timestamps)
            .locale_config(locale_config)
            .build()
    }

    /// The snapshot context of a global's single `default` row. Global tables
    /// always keep timestamps.
    pub(crate) fn for_global(
        gtable: &'a str,
        def: &'a GlobalDefinition,
        locale_config: Option<&'a LocaleConfig>,
    ) -> Self {
        Self::builder(gtable, "default")
            .fields(&def.fields)
            .versions(def.versions.as_ref())
            .has_drafts(def.has_drafts())
            .locale_config(locale_config)
            .build()
    }

    /// The main table the status write lands on.
    pub(in crate::service::versions) fn status_table(&self) -> StatusTable<'_> {
        StatusTable::new(self.table, self.timestamps)
    }
}

/// Set document status, create a version snapshot, and prune.
///
/// On a drafts-enabled definition the status write is the one place a
/// create/publish moves `_status`, so it also stamps that status onto `doc`:
/// the caller reads the row back before the status moves, and `doc` is what
/// its `after_change` hooks, its response and its live event are built from.
/// Without the stamp a draft create reported (and routed its event as)
/// `published`, and publishing a draft row reported `draft`.
///
/// # Errors
///
/// Returns a backend error if the status update, the snapshot build or the
/// version write fails.
pub(crate) fn create_version_snapshot(
    conn: &dyn DbConnection,
    ctx: &VersionSnapshotCtx<'_>,
    status: &str,
    doc: &mut Document,
) -> Result<()> {
    if ctx.has_drafts {
        query::set_document_status(conn, ctx.status_table(), ctx.parent_id, status)?;

        doc.fields
            .insert("_status".to_string(), Value::String(status.to_string()));
    }

    // A format converted on the background queue is not part of this write:
    // the upload pipeline blanks its column, so the row — and therefore this
    // snapshot — records it empty, and the job that later fills the row never
    // revisits the snapshot. The snapshot is recorded as it was true at the
    // time; a restore of it re-derives and re-queues whatever variant it left
    // empty (`restored_file_conversions`).
    let mut snapshot = query::build_snapshot(conn, ctx.table, ctx.fields, doc, ctx.locale_config)?;

    // The snapshot records the status this version is stamped with, whatever
    // shape `build_snapshot` gives the system columns. Reads treat the row as
    // the authority regardless, but a snapshot should not store a value that
    // was never true.
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
    #[cfg(feature = "sqlite")]
    use rusqlite::Connection;
    #[cfg(feature = "sqlite")]
    use serde_json::json;

    use super::*;
    #[cfg(feature = "sqlite")]
    use crate::core::{DocumentFields, FieldType};

    #[test]
    fn builder_defaults_to_empty_fields_no_config_no_drafts() {
        let ctx = VersionSnapshotCtx::builder("posts", "doc-1").build();
        assert_eq!(ctx.table, "posts");
        assert_eq!(ctx.parent_id, "doc-1");
        assert!(ctx.fields.is_empty());
        assert!(ctx.versions.is_none());
        assert!(!ctx.has_drafts);
        assert!(
            ctx.timestamps,
            "tables keep timestamps unless told otherwise"
        );
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

    /// A drafts-enabled `logs` table without timestamps, and its version table.
    #[cfg(feature = "sqlite")]
    fn logs_without_timestamps() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE logs (
                id TEXT PRIMARY KEY,
                title TEXT,
                _status TEXT DEFAULT 'published'
            );
            CREATE TABLE _versions_logs (
                id TEXT PRIMARY KEY,
                _parent TEXT NOT NULL,
                _version INTEGER NOT NULL,
                _status TEXT NOT NULL,
                _latest INTEGER NOT NULL DEFAULT 0,
                snapshot TEXT NOT NULL,
                created_at TEXT
            );
            INSERT INTO logs (id, title) VALUES ('l1', 'Entry');",
        )
        .unwrap();

        conn
    }

    /// The doc a create hands over is read back before the status write, so
    /// it still says `published`. The snapshot step moves the row AND the
    /// in-memory doc — on a table without timestamps, where the status write
    /// must not name `updated_at`.
    #[cfg(feature = "sqlite")]
    #[test]
    fn draft_snapshot_stamps_the_doc_and_skips_missing_timestamps() {
        let conn = logs_without_timestamps();
        let fields = vec![FieldDefinition::builder("title", FieldType::Text).build()];
        let vc = VersionsConfig::new(true, 0);

        let mut doc = Document::builder("l1")
            .fields(
                [
                    ("title".to_string(), json!("Entry")),
                    ("_status".to_string(), json!("published")),
                ]
                .into_iter()
                .collect::<DocumentFields>(),
            )
            .build();

        let ctx = VersionSnapshotCtx::builder("logs", "l1")
            .fields(&fields)
            .versions(Some(&vc))
            .has_drafts(true)
            .timestamps(false)
            .build();

        create_version_snapshot(&conn, &ctx, "draft", &mut doc).expect("snapshot");

        assert_eq!(doc.fields.get("_status"), Some(&json!("draft")));
        assert_eq!(
            query::get_document_status(&conn, "logs", "l1")
                .unwrap()
                .as_deref(),
            Some("draft")
        );
    }
}
