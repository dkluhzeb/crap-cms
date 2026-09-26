//! Unpublish with version snapshot creation.

use anyhow::Result;

use serde_json::Value;

use crate::{
    core::{Document, collection::VersionsConfig},
    db::{DbConnection, query, query::VersionWrite},
    service::{Def, ServiceContext, ServiceError, versions::VersionSnapshotCtx},
};

/// Refuse an unpublish on a definition without drafts.
///
/// Unpublishing moves `_status` to `draft`; a definition without drafts has no
/// such column, so the document would stay public while the operation reported
/// it unpublished, recorded a spurious draft version and announced an
/// `unpublish` event. Checked at the collection and global unpublish
/// chokepoints, which every surface reaches.
///
/// # Errors
///
/// Returns [`ServiceError::HookError`] when the definition has no drafts.
pub(crate) fn require_unpublish_capability(ctx: &ServiceContext) -> Result<(), ServiceError> {
    if ctx.has_drafts() {
        return Ok(());
    }

    let kind = match ctx.def {
        Def::Global(_) => "Global",
        _ => "Collection",
    };

    Err(ServiceError::HookError(format!(
        "{kind} '{}' does not support unpublish: versioning with drafts is not enabled",
        ctx.slug
    )))
}

/// Whether a pending draft already stands for this document's unpublished
/// content — the same "latest version is a draft" test the admin overlay, the
/// draft read view and the next publish resolve the pending draft with.
fn has_pending_draft(conn: &dyn DbConnection, table: &str, parent_id: &str) -> Result<bool> {
    Ok(query::find_latest_version(conn, table, parent_id)?
        .is_some_and(|version| version.status == "draft"))
}

/// Set a document's status to "draft" and record the unpublished content as a
/// version — unless a pending draft already holds it.
/// Used by both collection `persist_unpublish` and the globals unpublish core.
///
/// A pending draft IS the document's unpublished content: the admin overlay,
/// the draft read view and the next publish all resolve it as the latest
/// version. Snapshotting the live row on top of it would make a `draft` copy of
/// the PUBLISHED content the latest version instead — the editor's pending
/// changes would stop being what unpublishing exposes, and under a small
/// `max_versions` the draft's own row would be pruned away. So with a draft
/// pending the status flip is the whole operation; without one the live row is
/// snapshotted as the draft, which is what keeps the unpublished content
/// restorable.
///
/// `doc` is the stored row the caller read (under its row lock); it comes back
/// stamped `_status = "draft"`, the status the row ends with.
///
/// # Errors
///
/// Returns a backend error if the status update, the pending-draft lookup, the
/// snapshot build or the version write fails.
pub(crate) fn unpublish_with_snapshot(
    conn: &dyn DbConnection,
    ctx: &VersionSnapshotCtx<'_>,
    doc: &mut Document,
) -> Result<()> {
    // The service chokepoints refuse unpublish without drafts; a definition
    // without drafts has no `_status` column to write.
    if ctx.has_drafts {
        query::set_document_status(conn, ctx.status_table(), ctx.parent_id, "draft")?;

        doc.fields
            .insert("_status".to_string(), Value::String("draft".to_string()));
    }

    if has_pending_draft(conn, ctx.table, ctx.parent_id)? {
        return Ok(());
    }

    let snapshot = query::build_snapshot(conn, ctx.table, ctx.fields, doc, ctx.locale_config)?;

    query::create_version_and_prune(
        conn,
        &VersionWrite::builder(ctx.table, ctx.parent_id, "draft", &snapshot)
            .max_versions(VersionsConfig::cap(ctx.versions))
            .build(),
    )?;

    Ok(())
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use rusqlite::Connection;
    use serde_json::json;

    use super::*;
    use crate::core::{DocumentFields, FieldDefinition, FieldType};

    /// A published `posts` row with an empty version table, capped at one
    /// version so a superfluous snapshot would push the pending draft out.
    fn published_post() -> (Connection, Vec<FieldDefinition>, VersionsConfig) {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                _revision INTEGER NOT NULL DEFAULT 0,
                title TEXT,
                _status TEXT DEFAULT 'published',
                created_at TEXT,
                updated_at TEXT
            );
            CREATE TABLE _versions_posts (
                id TEXT PRIMARY KEY,
                _parent TEXT NOT NULL,
                _version INTEGER NOT NULL,
                _status TEXT NOT NULL,
                _latest INTEGER NOT NULL DEFAULT 0,
                snapshot TEXT NOT NULL,
                created_at TEXT
            );
            INSERT INTO posts (id, title, _status) VALUES ('p1', 'Published', 'published');",
        )
        .unwrap();

        (
            conn,
            vec![FieldDefinition::builder("title", FieldType::Text).build()],
            VersionsConfig::new(true, 1),
        )
    }

    /// The live row as the unpublish path hands it over.
    fn live_row() -> Document {
        let fields: DocumentFields = [("title".to_string(), json!("Published"))]
            .into_iter()
            .collect();

        Document::builder("p1").fields(fields).build()
    }

    fn snapshot_ctx<'a>(
        fields: &'a [FieldDefinition],
        versions: &'a VersionsConfig,
    ) -> VersionSnapshotCtx<'a> {
        VersionSnapshotCtx::builder("posts", "p1")
            .fields(fields)
            .versions(Some(versions))
            .has_drafts(true)
            .build()
    }

    fn status(conn: &Connection) -> Option<String> {
        query::get_document_status(conn, "posts", "p1").unwrap()
    }

    /// A pending draft IS the unpublished content. Unpublishing used to
    /// snapshot the LIVE row over it, so the editor's draft stopped being what
    /// the draft view served and — under a one-version cap — was pruned away.
    #[test]
    fn unpublishing_keeps_the_pending_draft() {
        let (conn, fields, versions) = published_post();
        query::create_version(
            &conn,
            "posts",
            "p1",
            "draft",
            &json!({ "title": "Drafted" }),
        )
        .unwrap();

        unpublish_with_snapshot(&conn, &snapshot_ctx(&fields, &versions), &mut live_row()).unwrap();

        let latest = query::find_latest_version(&conn, "posts", "p1")
            .unwrap()
            .expect("a version");

        assert_eq!(latest.status, "draft");
        assert_eq!(
            latest.snapshot["title"],
            json!("Drafted"),
            "the pending draft is still what unpublishing exposes"
        );
        assert_eq!(
            query::count_versions(&conn, "posts", "p1", false).unwrap(),
            1,
            "no copy of the published row was recorded"
        );
        assert_eq!(status(&conn).as_deref(), Some("draft"));
    }

    /// Without a pending draft the live row is snapshotted, which is what keeps
    /// the unpublished content restorable.
    #[test]
    fn unpublishing_without_a_pending_draft_snapshots_the_live_row() {
        let (conn, fields, versions) = published_post();

        unpublish_with_snapshot(&conn, &snapshot_ctx(&fields, &versions), &mut live_row()).unwrap();

        let latest = query::find_latest_version(&conn, "posts", "p1")
            .unwrap()
            .expect("a version");

        assert_eq!(latest.status, "draft");
        assert_eq!(latest.snapshot["title"], json!("Published"));
        assert_eq!(status(&conn).as_deref(), Some("draft"));
    }

    /// A published latest version is not a pending draft, so the unpublish
    /// records one of its own.
    #[test]
    fn unpublishing_after_a_publish_records_a_draft_version() {
        let (conn, fields, versions) = published_post();
        query::create_version(
            &conn,
            "posts",
            "p1",
            "published",
            &json!({ "title": "Published" }),
        )
        .unwrap();

        unpublish_with_snapshot(&conn, &snapshot_ctx(&fields, &versions), &mut live_row()).unwrap();

        assert_eq!(
            query::find_latest_version(&conn, "posts", "p1")
                .unwrap()
                .expect("a version")
                .status,
            "draft"
        );
    }
}
