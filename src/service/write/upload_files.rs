//! What an upload-collection write settles besides the row: the stored files
//! the row stops referencing, and the image conversions queued against them.
//!
//! Every part of it runs INSIDE the write transaction. The job rows are
//! inserted on the write's own connection, so a crash between the commit and a
//! second connection's insert can no longer leave a derivative URL unfilled
//! forever; the file deletions are only *queued* here and drained by
//! [`run_pool_write`] after the commit, so a rolled-back write never deletes
//! bytes a live row still points at.
//!
//! Which files go is decided by difference, not by "did this request carry a
//! file": the keys the document referenced before the write, minus the keys it
//! references after it. A stored file is deleted when, and only when, no live
//! row, draft or version snapshot of its document references it once the write
//! commits — so a replaced file whose bytes an older version still needs stays
//! until that version is pruned, and a drafted file that was superseded before
//! it ever went live goes as soon as the snapshot naming it is gone.
//!
//! [`run_pool_write`]: crate::service::run_pool_write

use std::collections::HashSet;

use tracing::warn;

use crate::{
    core::{
        Builder, CollectionDefinition, Document, DocumentFields,
        upload::{enqueue_conversions, snapshot_file_keys, upload_file_entries, upload_file_keys},
    },
    db::{DbConnection, LocaleContext, query},
    service::{ServiceContext, ServiceError, UploadConversions, write::cancel_image_jobs},
};

type Result<T> = std::result::Result<T, ServiceError>;

/// The stored files one document referenced before a write, split by what holds
/// the reference.
///
/// The split matters after the write: the published row's keys are re-derived
/// from the row the write produced, while a write that left the published row
/// alone (a draft-only save) still references exactly what it did going in.
pub(crate) struct DocumentFiles {
    /// Keys the published row referenced.
    pub live: Vec<String>,
    /// Keys any version snapshot — draft or published — referenced.
    pub snapshots: Vec<String>,
}

impl DocumentFiles {
    pub(crate) fn new(live: Vec<String>, snapshots: Vec<String>) -> Self {
        Self { live, snapshots }
    }

    /// Every key the document referenced, wherever the reference lived.
    fn all(&self) -> impl Iterator<Item = &String> {
        self.live.iter().chain(&self.snapshots)
    }
}

/// Every storage key one document owns — the published row's AND every version
/// snapshot's, each listed once.
///
/// What a hard delete has to remove: a file only a never-published draft ever
/// named is still this document's file, and once the row and its versions are
/// gone nothing can reference it again, so leaving it behind leaks it forever.
/// The one answer shared by the service delete, the CLI trash purge and the
/// scheduled retention purge, so the three cannot drift.
///
/// The row is read UNFILTERED, unlike the live-write path's: a purge's target
/// is usually a trashed row, which a filtered read reports as gone — and a
/// delete that believes the row is gone deletes none of its files. Empty (and
/// no query issued) for a collection without uploads.
///
/// # Errors
///
/// Returns a backend error if the row or the snapshots cannot be read. It must
/// propagate: a swallowed error leaks every file the document owns.
pub(crate) fn owned_file_keys(
    conn: &dyn DbConnection,
    def: &CollectionDefinition,
    id: &str,
    locale_ctx: Option<&LocaleContext>,
) -> Result<Vec<String>> {
    let Some(upload) = def.upload.as_ref().filter(|u| u.enabled) else {
        return Ok(Vec::new());
    };

    let slug = &def.slug;
    let live = query::find_by_id_unfiltered(conn, slug, def, id, locale_ctx)?
        .map(|doc| upload_file_keys(&doc.fields, upload))
        .unwrap_or_default();

    let mut seen = HashSet::new();

    Ok(live
        .into_iter()
        .chain(snapshot_keys(conn, slug, def, id)?)
        .filter(|key| seen.insert(key.clone()))
        .collect())
}

/// Read the published row for upload purposes.
///
/// The file columns are shared across locales, but a localized collection still
/// needs *some* context for the SELECT to name `caption__en` rather than a bare
/// `caption` that does not exist — hence the fallback to the default locale
/// when the write carries no context of its own.
///
/// # Errors
///
/// Returns a backend error if the row cannot be read.
pub(in crate::service::write) fn stored_row(
    ctx: &ServiceContext,
    def: &CollectionDefinition,
    id: &str,
    locale_ctx: Option<&LocaleContext>,
) -> Result<Option<Document>> {
    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();

    let fallback = if locale_ctx.is_none() {
        ctx.default_locale_ctx()
    } else {
        None
    };

    Ok(query::find_by_id(
        conn,
        ctx.slug,
        def,
        id,
        locale_ctx.or(fallback.as_ref()),
    )?)
}

/// The storage keys the document references right now — the published row's and
/// every version snapshot's.
///
/// Read before the write so the keys nothing references any more can be
/// resolved afterwards by difference. Empty (and no query issued) for a
/// collection without uploads.
///
/// # Errors
///
/// Returns a backend error if the row or the snapshots cannot be read. It must
/// propagate: swallowing it would silently skip the cleanup and leak every file
/// the write replaces.
pub(crate) fn document_file_keys(
    ctx: &ServiceContext,
    def: &CollectionDefinition,
    id: &str,
    locale_ctx: Option<&LocaleContext>,
) -> Result<DocumentFiles> {
    let Some(upload) = def.upload.as_ref().filter(|u| u.enabled) else {
        return Ok(DocumentFiles::new(Vec::new(), Vec::new()));
    };

    let live = stored_row(ctx, def, id, locale_ctx)?
        .map(|doc| upload_file_keys(&doc.fields, upload))
        .unwrap_or_default();

    let conn = ctx.resolve_conn()?;

    Ok(DocumentFiles::new(
        live,
        snapshot_keys(conn.as_ref(), ctx.slug, def, id)?,
    ))
}

/// The storage keys every version snapshot of the document references.
///
/// Each snapshot stands in for a row, so its keys are derived with the same
/// rule a row's are — a restore has to find the bytes the snapshot names.
/// Empty (and no query issued) for a collection without versions.
fn snapshot_keys(
    conn: &dyn DbConnection,
    slug: &str,
    def: &CollectionDefinition,
    id: &str,
) -> Result<Vec<String>> {
    let Some(upload) = def.upload.as_ref().filter(|u| u.enabled) else {
        return Ok(Vec::new());
    };

    if !def.has_versions() {
        return Ok(Vec::new());
    }

    let mut keys = Vec::new();

    for snapshot in query::list_snapshots(conn, slug, id)? {
        keys.extend(snapshot_file_keys(&snapshot, upload));
    }

    Ok(keys)
}

/// The columns the conversions this write queued are going to overwrite when
/// they run.
fn pending_url_columns(conversions: Option<&UploadConversions>) -> HashSet<&str> {
    conversions
        .into_iter()
        .flat_map(|c| c.queued.iter().map(|q| q.url_column.as_str()))
        .collect()
}

/// The keys the published row references once this write has landed.
///
/// A column a queued conversion is about to overwrite does NOT count as a
/// reference: only formats converted synchronously are part of the write, so a
/// queued one leaves the PREVIOUS file's derivative url standing in its column
/// until the job runs and replaces it. Counting that stale url as kept would
/// leave the previous derivative's bytes in storage with nothing referencing
/// them once the job lands.
fn row_keys(
    after_fields: &DocumentFields,
    def: &CollectionDefinition,
    conversions: Option<&UploadConversions>,
) -> Vec<String> {
    let Some(upload) = def.upload.as_ref() else {
        return Vec::new();
    };

    let pending = pending_url_columns(conversions);

    upload_file_entries(after_fields, upload)
        .into_iter()
        .filter(|(column, _)| !pending.contains(column))
        .map(|(_, key)| key)
        .collect()
}

/// The keys `before` named that `kept` no longer does, each once.
fn unreferenced(before: &DocumentFiles, kept: &[String]) -> Vec<String> {
    let mut dropped: Vec<String> = Vec::new();

    for key in before.all() {
        if !kept.contains(key) && !dropped.contains(key) {
            dropped.push(key.clone());
        }
    }

    dropped
}

/// The keys this write leaves with nothing referencing them.
fn dropped_keys(
    ctx: &ServiceContext,
    conn: &dyn DbConnection,
    settle: &UploadSettle<'_>,
) -> Result<Vec<String>> {
    // A create had nothing stored, so it can drop nothing.
    let Some(before) = settle.before else {
        return Ok(Vec::new());
    };

    let mut kept = match settle.updated_row {
        Some(fields) => row_keys(fields, settle.def, settle.conversions),
        // A draft-only save left the published row — and every key it named —
        // exactly as it was.
        None => before.live.clone(),
    };

    // The surviving snapshots, read after the write: one this write pruned no
    // longer holds its file back.
    kept.extend(snapshot_keys(conn, ctx.slug, settle.def, settle.id)?);

    Ok(unreferenced(before, &kept))
}

/// One upload write's file/job aftermath, settled inside its transaction.
#[derive(Builder)]
pub(crate) struct UploadSettle<'a> {
    #[builder(required)]
    pub def: &'a CollectionDefinition,
    #[builder(required)]
    pub id: &'a str,
    /// The files the document referenced before the write
    /// ([`document_file_keys`]). `None` on a create — there was no document.
    pub before: Option<&'a DocumentFiles>,
    /// The published row as an *update* left it.
    ///
    /// `None` when this write replaced no existing published row — a create
    /// (there was none) or a draft-only save (it wrote a version snapshot and
    /// left the published row, its files and their queued conversions alone).
    /// Both the dropped-file diff and the stale-conversion cancel key on it.
    pub updated_row: Option<&'a DocumentFields>,
    /// The conversions the file this write makes live still owes; `None` when
    /// the write changed no file.
    pub conversions: Option<&'a UploadConversions>,
}

/// Settle the files and jobs of an upload write that is about to commit.
///
/// - Queues every file the document stopped referencing — live row, draft and
///   version snapshots together — for post-commit deletion.
/// - Cancels the previous file's still-queued conversions when this write
///   replaced the published row's file — such a job writes its derivative URL
///   onto the document when it runs, so left in the queue it lands after the
///   new file's own conversions and overwrites them with a derivative of a file
///   the document no longer references.
/// - Queues the conversions the newly live file still owes.
///
/// # Errors
///
/// Returns a backend error if the surviving snapshots cannot be read or the
/// conversion jobs cannot be inserted — they are part of the write, not a
/// best-effort afterthought.
pub(crate) fn settle_upload_write(ctx: &ServiceContext, settle: &UploadSettle<'_>) -> Result<()> {
    if !settle.def.is_upload_collection() {
        return Ok(());
    }

    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();

    queue_for_deletion(ctx, dropped_keys(ctx, conn, settle)?);

    let Some(conversions) = settle.conversions else {
        return Ok(());
    };

    if settle.updated_row.is_some() {
        cancel_image_jobs(conn, ctx.slug, settle.def, settle.id);
    }

    enqueue_conversions(
        conn,
        ctx.slug,
        settle.id,
        &conversions.queued,
        conversions.max_attempts,
    )?;

    Ok(())
}

/// One wording for every place a write's dropped files cannot be deleted:
/// whichever half of the cleanup path is `missing`, the bytes stay behind with
/// nothing referencing them. Silence is what makes such a leak invisible, so
/// the count is always named.
pub(crate) fn warn_orphaned_files(slug: &str, missing: &str, count: usize) {
    if count == 0 {
        return;
    }

    warn!("No {missing} for '{slug}': {count} orphaned upload file(s) left in storage");
}

/// Hand the keys to the post-commit cleanup queue. Without a queue (a Lua CRUD
/// write inside a hook transaction the service does not own) the bytes stay:
/// an orphaned file is the safe direction, a file deleted for a write that then
/// rolls back is not.
fn queue_for_deletion(ctx: &ServiceContext, keys: Vec<String>) {
    if keys.is_empty() {
        return;
    }

    let Some(queue) = ctx.file_cleanup.as_ref() else {
        warn_orphaned_files(ctx.slug, "post-commit file cleanup queue", keys.len());

        return;
    };

    queue.borrow_mut().extend(keys);
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::core::upload::{
        CollectionUpload, FormatQuality, ImageSizeBuilder, QueuedConversion,
    };

    fn media_def() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("media");
        def.upload = Some(CollectionUpload::new());

        def
    }

    /// `media` with a `thumbnail` size whose webp variant is a real column.
    fn media_with_thumbnail_webp() -> CollectionDefinition {
        let mut upload = CollectionUpload::new();
        upload.image_sizes = vec![
            ImageSizeBuilder::new("thumbnail")
                .width(300)
                .height(300)
                .build(),
        ];
        upload.format_options.webp = Some(FormatQuality::new(80, false));

        let mut def = CollectionDefinition::new("media");
        def.upload = Some(upload);

        def
    }

    fn fields_with_url(url: &str) -> DocumentFields {
        [("url".to_string(), json!(url))].into_iter().collect()
    }

    /// The row a replacement leaves behind when the thumbnail's webp variant is
    /// converted asynchronously: a new `url`, and the PREVIOUS file's webp url
    /// still standing in the column the queued job will overwrite.
    fn replaced_row(url: &str, stale_webp: &str) -> DocumentFields {
        [
            ("url".to_string(), json!(url)),
            ("thumbnail_webp_url".to_string(), json!(stale_webp)),
        ]
        .into_iter()
        .collect()
    }

    fn queued_webp(target: &str) -> UploadConversions {
        UploadConversions::new(
            vec![QueuedConversion {
                source_path: "media/b.png".to_string(),
                target_path: target.to_string(),
                format: "webp".to_string(),
                quality: 80,
                url_column: "thumbnail_webp_url".to_string(),
                url_value: format!("/uploads/{target}"),
            }],
            1,
        )
    }

    /// The published row alone referenced the key, and no longer does.
    fn live_only(keys: &[&str]) -> DocumentFiles {
        DocumentFiles::new(keys.iter().map(|k| (*k).to_string()).collect(), Vec::new())
    }

    /// Replacing the file drops exactly the key the row stopped referencing.
    #[test]
    fn a_replaced_file_is_the_dropped_key() {
        let def = media_def();
        let before = live_only(&["old.png"]);

        let kept = row_keys(&fields_with_url("/uploads/new.png"), &def, None);

        assert_eq!(unreferenced(&before, &kept), vec!["old.png".to_string()]);
    }

    /// Regression: a draft save with a new file must NOT drop the published
    /// file. The draft write leaves the published row pointing at the current
    /// file, so deleting it orphaned the live document (broken image) — the old
    /// rule keyed the deletion on "a file was uploaded", not on what the row
    /// ended up referencing.
    #[test]
    fn a_row_that_kept_its_file_drops_nothing() {
        let def = media_def();
        let before = live_only(&["old.png"]);

        let kept = row_keys(&fields_with_url("/uploads/old.png"), &def, None);

        let dropped = unreferenced(&before, &kept);
        assert!(dropped.is_empty(), "{dropped:?}");
    }

    /// A key a version snapshot still names is kept even though the published
    /// row dropped it: restoring that version has to find the bytes.
    #[test]
    fn a_key_a_snapshot_references_is_not_dropped() {
        let def = media_def();
        let before = DocumentFiles::new(vec!["old.png".to_string()], vec!["old.png".to_string()]);

        // The surviving snapshot still names it, so it is part of `kept`.
        let mut kept = row_keys(&fields_with_url("/uploads/new.png"), &def, None);
        kept.push("old.png".to_string());

        assert!(unreferenced(&before, &kept).is_empty());
    }

    /// A drafted file the published row never referenced is dropped once the
    /// snapshot that named it is gone — a newer draft superseded it and the cap
    /// pruned the older one.
    #[test]
    fn a_superseded_draft_file_is_dropped_when_its_snapshot_is_gone() {
        let before = DocumentFiles::new(
            vec!["published.png".to_string()],
            vec!["published.png".to_string(), "drafted.png".to_string()],
        );

        // After the write: the published row and the surviving snapshot both
        // name the published file; nothing names the superseded draft's.
        let kept = vec!["published.png".to_string(), "published.png".to_string()];

        assert_eq!(
            unreferenced(&before, &kept),
            vec!["drafted.png".to_string()]
        );
    }

    /// A key both the row and a snapshot named is reported once, not twice.
    #[test]
    fn a_key_referenced_twice_is_dropped_once() {
        let before = DocumentFiles::new(vec!["old.png".to_string()], vec!["old.png".to_string()]);

        assert_eq!(unreferenced(&before, &[]), vec!["old.png".to_string()]);
    }

    /// Publishing a draft that swapped the file carries no file of its own, yet
    /// the published row now points elsewhere — the previous file is dropped.
    #[test]
    fn publishing_a_swapped_file_drops_the_previous_one() {
        let def = media_def();
        let before = live_only(&["published.png"]);

        let kept = row_keys(&fields_with_url("/uploads/drafted.png"), &def, None);

        assert_eq!(
            unreferenced(&before, &kept),
            vec!["published.png".to_string()]
        );
    }

    /// Regression: an asynchronously converted format is NOT part of the write
    /// — `inject_upload_metadata` only writes the formats converted inline — so
    /// the re-read row still carries the previous file's `thumbnail_webp_url`.
    /// Counting that stale url as "still referenced" kept the previous
    /// derivative out of the diff; the queued job then overwrote the column and
    /// its bytes were unreferenced and undeletable forever.
    #[test]
    fn a_pending_conversion_column_does_not_keep_the_previous_derivative() {
        let def = media_with_thumbnail_webp();
        let before = live_only(&["media/a.png", "media/a_thumbnail.webp"]);

        let after = replaced_row("/uploads/media/b.png", "/uploads/media/a_thumbnail.webp");

        let kept = row_keys(&after, &def, Some(&queued_webp("media/b_thumbnail.webp")));

        let mut dropped = unreferenced(&before, &kept);
        dropped.sort();

        assert_eq!(
            dropped,
            vec![
                "media/a.png".to_string(),
                "media/a_thumbnail.webp".to_string()
            ]
        );
    }

    /// The exclusion is per column, not blanket: a derivative column no queued
    /// conversion targets is a live reference, so its file stays. (An upload
    /// whose formats were all converted inline must not lose its derivatives.)
    #[test]
    fn a_column_without_a_pending_conversion_still_keeps_its_file() {
        let def = media_with_thumbnail_webp();
        let before = live_only(&["media/a.png", "media/b_thumbnail.webp"]);

        // The write itself filled the webp column — the new file's derivative.
        let after = replaced_row("/uploads/media/b.png", "/uploads/media/b_thumbnail.webp");

        assert_eq!(
            unreferenced(&before, &row_keys(&after, &def, None)),
            vec!["media/a.png".to_string()]
        );
    }

    /// A write that queued nothing for a column leaves that column's file
    /// alone even when the write DID queue other conversions.
    #[test]
    fn an_unrelated_pending_conversion_does_not_drop_another_column() {
        let mut def = media_with_thumbnail_webp();
        let upload = def.upload.as_mut().expect("upload");
        upload
            .image_sizes
            .push(ImageSizeBuilder::new("card").width(640).height(480).build());

        let before = live_only(&["media/a_card.webp"]);

        let after: DocumentFields = [(
            "card_webp_url".to_string(),
            json!("/uploads/media/a_card.webp"),
        )]
        .into_iter()
        .collect();

        // The queued conversion targets `thumbnail_webp_url`, not `card_webp_url`.
        let kept = row_keys(&after, &def, Some(&queued_webp("media/b_thumbnail.webp")));

        assert!(unreferenced(&before, &kept).is_empty());
    }

    /// A collection without uploads never resolves a key, whatever its columns
    /// look like.
    #[test]
    fn a_non_upload_collection_drops_nothing() {
        let def = CollectionDefinition::new("posts");
        let before = live_only(&["old.png"]);

        let kept = row_keys(&fields_with_url("/uploads/new.png"), &def, None);

        // Nothing resolves to a key, so nothing was ever referenced either —
        // the settle short-circuits on `is_upload_collection` before this.
        assert!(kept.is_empty());
        assert_eq!(unreferenced(&before, &kept), vec!["old.png".to_string()]);
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod settle_tests {
    use std::{cell::RefCell, rc::Rc};

    use rusqlite::Connection;
    use serde_json::json;

    use super::*;
    use crate::{
        core::upload::{
            CollectionUpload, FALLBACK_MAX_ATTEMPTS, FormatQuality, ImageConvertJobData,
            ImageSizeBuilder, QueuedConversion, queue_image_conversion,
        },
        db::{migrate, query::jobs::list_job_runs},
        hooks::lifecycle::FileCleanupQueue,
    };

    /// An in-memory database holding only the jobs table.
    fn jobs_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        migrate::create_jobs_table(&conn, "TEXT DEFAULT (datetime('now'))", "TEXT")
            .expect("create_jobs_table");

        conn
    }

    fn media_def() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("media");
        def.upload = Some(CollectionUpload::new());

        def
    }

    /// `media` with the `thumbnail` size whose webp variant is a real column,
    /// so `thumbnail_webp_url` counts as a server-derived file column.
    fn media_with_thumbnail_webp() -> CollectionDefinition {
        let mut upload = CollectionUpload::new();
        upload.image_sizes = vec![
            ImageSizeBuilder::new("thumbnail")
                .width(300)
                .height(300)
                .build(),
        ];
        upload.format_options.webp = Some(FormatQuality::new(80, false));

        let mut def = CollectionDefinition::new("media");
        def.upload = Some(upload);

        def
    }

    fn conversion(target: &str) -> QueuedConversion {
        QueuedConversion {
            source_path: "a.png".to_string(),
            target_path: target.to_string(),
            format: "webp".to_string(),
            quality: 80,
            url_column: "thumbnail_webp_url".to_string(),
            url_value: format!("/uploads/{target}"),
        }
    }

    fn queue_old_conversion(conn: &Connection) {
        queue_image_conversion(
            conn,
            &ImageConvertJobData {
                collection: "media".to_string(),
                document_id: "m1".to_string(),
                source_path: "old.png".to_string(),
                target_path: "old.webp".to_string(),
                format: "webp".to_string(),
                quality: 80,
                url_column: "thumbnail_webp_url".to_string(),
                url_value: "/uploads/old.webp".to_string(),
            },
            FALLBACK_MAX_ATTEMPTS,
        )
        .unwrap();
    }

    /// The queued payloads of every job still in the queue.
    fn queued_payloads(conn: &Connection) -> Vec<String> {
        list_job_runs(conn, None, None, 100, 0)
            .unwrap()
            .into_iter()
            .map(|run| run.data)
            .collect()
    }

    fn row(url: &str) -> DocumentFields {
        [("url".to_string(), json!(url))].into_iter().collect()
    }

    /// The published row alone referenced these keys; the collection in these
    /// tests has no versions, so no snapshot holds anything back.
    fn live(keys: &[&str]) -> DocumentFiles {
        DocumentFiles::new(keys.iter().map(|k| (*k).to_string()).collect(), Vec::new())
    }

    /// Regression: the admin file replacement queued the new conversions but
    /// left the previous file's queued. The stale run finishes later and writes
    /// its derivative URL over the new file's — the document then points at a
    /// derivative of a file it no longer references.
    #[test]
    fn replacing_a_file_cancels_the_previous_files_conversions() {
        let conn = jobs_db();
        let def = media_def();
        queue_old_conversion(&conn);

        let ctx = ServiceContext::collection("media", &def)
            .conn(&conn)
            .build();
        let before = live(&["old.png"]);
        let conversions = UploadConversions::new(vec![conversion("new.webp")], 1);

        settle_upload_write(
            &ctx,
            &UploadSettle::builder(&def, "m1")
                .before(Some(&before))
                .updated_row(Some(&row("/uploads/new.png")))
                .conversions(Some(&conversions))
                .build(),
        )
        .expect("settle");

        let payloads = queued_payloads(&conn);
        assert_eq!(payloads.len(), 1, "{payloads:?}");
        assert!(payloads[0].contains("new.webp"), "{payloads:?}");
    }

    /// Regression: a draft save with a new file must not disturb the published
    /// row — neither its file nor the conversions still queued for that file.
    /// The drafted file's own conversions wait for the publish that makes it
    /// live, so the draft save carries none.
    #[test]
    fn a_draft_save_keeps_the_published_files_conversions_and_file() {
        let conn = jobs_db();
        let def = media_def();
        queue_old_conversion(&conn);

        let cleanup: FileCleanupQueue = Rc::new(RefCell::new(Vec::new()));
        let ctx = ServiceContext::collection("media", &def)
            .conn(&conn)
            .file_cleanup(cleanup.clone())
            .build();
        let before = live(&["old.png"]);

        settle_upload_write(
            &ctx,
            &UploadSettle::builder(&def, "m1")
                .before(Some(&before))
                .build(),
        )
        .expect("settle");

        let payloads = queued_payloads(&conn);
        assert_eq!(
            payloads.len(),
            1,
            "the published file's conversion stays queued: {payloads:?}"
        );
        assert!(
            cleanup.borrow().is_empty(),
            "a draft save deletes no published file: {:?}",
            cleanup.borrow()
        );
    }

    /// The bytes of a replaced file go on the post-commit queue, never straight
    /// to the storage backend: the write can still roll back.
    #[test]
    fn a_replaced_file_is_queued_for_post_commit_deletion() {
        let conn = jobs_db();
        let def = media_def();

        let cleanup: FileCleanupQueue = Rc::new(RefCell::new(Vec::new()));
        let ctx = ServiceContext::collection("media", &def)
            .conn(&conn)
            .file_cleanup(cleanup.clone())
            .build();
        let before = live(&["old.png"]);

        settle_upload_write(
            &ctx,
            &UploadSettle::builder(&def, "m1")
                .before(Some(&before))
                .updated_row(Some(&row("/uploads/new.png")))
                .build(),
        )
        .expect("settle");

        assert_eq!(*cleanup.borrow(), vec!["old.png".to_string()]);
        assert!(
            queued_payloads(&conn).is_empty(),
            "a write with no file of its own queues no conversion"
        );
    }

    /// Regression: the webp variant of a replacement is produced by a QUEUED
    /// job, so the write never sets `thumbnail_webp_url` and the stored row
    /// still points at the previous file's derivative. That stale url used to
    /// count as a live reference, so the derivative was never queued for
    /// deletion — and once the job overwrote the column, nothing referenced
    /// those bytes and nothing could ever delete them.
    #[test]
    fn a_queued_format_drops_the_previous_files_derivative() {
        let conn = jobs_db();
        let def = media_with_thumbnail_webp();

        let cleanup: FileCleanupQueue = Rc::new(RefCell::new(Vec::new()));
        let ctx = ServiceContext::collection("media", &def)
            .conn(&conn)
            .file_cleanup(cleanup.clone())
            .build();

        let before = live(&["media/a.png", "media/a_thumb.webp"]);
        let conversions = UploadConversions::new(vec![conversion("media/b_thumb.webp")], 1);

        let after: DocumentFields = [
            ("url".to_string(), json!("/uploads/media/b.png")),
            (
                "thumbnail_webp_url".to_string(),
                json!("/uploads/media/a_thumb.webp"),
            ),
        ]
        .into_iter()
        .collect();

        settle_upload_write(
            &ctx,
            &UploadSettle::builder(&def, "m1")
                .before(Some(&before))
                .updated_row(Some(&after))
                .conversions(Some(&conversions))
                .build(),
        )
        .expect("settle");

        let mut queued = cleanup.borrow().clone();
        queued.sort();

        assert_eq!(
            queued,
            vec!["media/a.png".to_string(), "media/a_thumb.webp".to_string()]
        );
    }

    /// A write that changes no file leaves the document's queued conversions
    /// alone: cancelling is what a REPLACEMENT owes the previous file, and an
    /// edit to an unrelated field must not strip a pending variant off the file
    /// the row still references.
    #[test]
    fn a_write_without_a_file_keeps_the_queued_conversions() {
        let conn = jobs_db();
        let def = media_def();
        queue_old_conversion(&conn);

        let ctx = ServiceContext::collection("media", &def)
            .conn(&conn)
            .build();
        let before = live(&["old.png"]);

        settle_upload_write(
            &ctx,
            &UploadSettle::builder(&def, "m1")
                .before(Some(&before))
                .updated_row(Some(&row("/uploads/new.png")))
                .build(),
        )
        .expect("settle");

        assert_eq!(queued_payloads(&conn).len(), 1);
    }
}
