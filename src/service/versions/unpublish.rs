//! Unpublish with version snapshot creation.

use anyhow::Result;

use crate::{
    config::LocaleConfig,
    core::{Document, FieldDefinition, collection::VersionsConfig},
    db::{DbConnection, query, query::VersionWrite},
};

/// Whether a pending draft already stands for this document's unpublished
/// content — the same "latest version is a draft" test the admin overlay, the
/// draft read view and the next publish resolve the pending draft with.
fn has_pending_draft(conn: &dyn DbConnection, table: &str, parent_id: &str) -> Result<bool> {
    Ok(query::find_latest_version(conn, table, parent_id)?
        .is_some_and(|version| version.status == "draft"))
}

/// Set a document's status to "draft" and record the unpublished content as a
/// version — unless a pending draft already holds it.
/// Used by both collection `persist_unpublish` and the globals unpublish handler.
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
/// # Errors
///
/// Returns a backend error if the status update, the pending-draft lookup, the
/// snapshot build or the version write fails.
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

    if has_pending_draft(conn, table, parent_id)? {
        return Ok(());
    }

    let snapshot = query::build_snapshot(conn, table, fields, doc, locale_config)?;

    query::create_version_and_prune(
        conn,
        &VersionWrite::builder(table, parent_id, "draft", &snapshot)
            .max_versions(VersionsConfig::cap(versions))
            .build(),
    )?;

    Ok(())
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use rusqlite::Connection;
    use serde_json::json;

    use super::*;
    use crate::core::{DocumentFields, FieldType};

    /// A published `posts` row with an empty version table, capped at one
    /// version so a superfluous snapshot would push the pending draft out.
    fn published_post() -> (Connection, Vec<FieldDefinition>, VersionsConfig) {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
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

        unpublish_with_snapshot(
            &conn,
            "posts",
            "p1",
            &fields,
            Some(&versions),
            &live_row(),
            None,
        )
        .unwrap();

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

        unpublish_with_snapshot(
            &conn,
            "posts",
            "p1",
            &fields,
            Some(&versions),
            &live_row(),
            None,
        )
        .unwrap();

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

        unpublish_with_snapshot(
            &conn,
            "posts",
            "p1",
            &fields,
            Some(&versions),
            &live_row(),
            None,
        )
        .unwrap();

        assert_eq!(
            query::find_latest_version(&conn, "posts", "p1")
                .unwrap()
                .expect("a version")
                .status,
            "draft"
        );
    }
}
