//! What a CLI purge of the trash did: the documents it skipped, the files it
//! leaves for after the commit, and the delete events it owes.

use anyhow::Result;

use crate::{
    cli,
    config::LocaleConfig,
    core::{CollectionDefinition, upload, upload::StorageBackend},
    db::DbConnection,
    service::{AppInfra, PurgeEvents, TrashedDoc, TrashedPurge},
};

/// Delete the files of the uploads a committed purge removed.
pub(super) fn delete_purged_files(storage: &dyn StorageBackend, purged: &Purged) {
    upload::delete_storage_keys(storage, &purged.upload_keys);
}

/// How many documents a purge deleted, and how many it skipped by reason.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct PurgeTally {
    /// Hard-deleted.
    pub(super) purged: u64,
    /// Still referenced by other documents.
    pub(super) referenced: u64,
    /// No longer in the trash when the purge reached them (restored, or
    /// trashed again too recently, since the candidates were read).
    pub(super) gone: u64,
}

impl PurgeTally {
    /// Add another collection's tally to this one.
    pub(super) fn add(&mut self, other: Self) {
        self.purged += other.purged;
        self.referenced += other.referenced;
        self.gone += other.gone;
    }
}

/// What purging documents did, and the delete events it owes once it has
/// committed.
pub(super) struct Purged {
    /// The documents deleted and skipped so far.
    pub(super) tally: PurgeTally,
    /// Every storage key the purged uploads owned — each document's row AND
    /// its version snapshots — whose files go once the purge commits.
    pub(super) upload_keys: Vec<String>,
    /// The purged documents' delete events, captured when there is a
    /// transport to publish them on.
    events: PurgeEvents,
}

impl Purged {
    pub(super) fn new(infra: Option<&AppInfra>) -> Self {
        let capture = infra.is_some_and(|i| i.event_transport.is_some());

        Self::with_events(PurgeEvents::new(capture))
    }

    fn with_events(events: PurgeEvents) -> Self {
        Self {
            tally: PurgeTally::default(),
            upload_keys: Vec::new(),
            events,
        }
    }

    /// Permanently delete trashed documents through the purge of the trash
    /// every caller shares (see [`PurgeEvents::purge_trashed`]): each is
    /// locked and re-checked first, so one restored since the candidates were
    /// read, or still referenced by others (`_ref_count > 0` — the delete
    /// protection the server surfaces enforce), is skipped. Collects the
    /// storage keys the purged uploads owned for deletion after the commit,
    /// and each document's delete event.
    pub(super) fn purge(
        &mut self,
        tx: &dyn DbConnection,
        def: &CollectionDefinition,
        docs: &[TrashedDoc<'_>],
        locale: &LocaleConfig,
    ) -> Result<()> {
        for doc in docs {
            let outcome = self.events.purge_trashed(tx, def, *doc, locale)?;

            self.record(&def.slug, doc.id(), outcome);
        }

        Ok(())
    }

    /// Count one document's outcome, warning about a skip.
    fn record(&mut self, slug: &str, id: &str, outcome: TrashedPurge) {
        match outcome {
            TrashedPurge::Purged(keys) => {
                self.tally.purged += 1;
                self.upload_keys.extend(keys);
            }
            TrashedPurge::Referenced(_) => {
                cli::warning(&format!(
                    "Skipping {slug} / {id} — still referenced by other documents"
                ));
                self.tally.referenced += 1;
            }
            TrashedPurge::NotTrashed => {
                cli::warning(&format!("Skipping {slug} / {id} — no longer in the trash"));
                self.tally.gone += 1;
            }
        }
    }

    /// Publish the purged documents' delete events. Call only once the purge
    /// has committed.
    pub(super) fn publish(self, infra: Option<&AppInfra>) {
        let Some(infra) = infra else { return };

        self.events.settle(infra);
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::{
        commands::trash::{find_purge_candidates, tests::setup_db},
        config::CrapConfig,
        core::{
            JobStatus,
            field::{FieldDefinition, FieldType, RelationshipConfig},
            upload::CollectionUpload,
        },
        db::{DbValue, query},
    };

    fn defs_with_relationship() -> (CollectionDefinition, CollectionDefinition) {
        let mut media = CollectionDefinition::new("media");
        media.soft_delete = true;
        let mut posts = CollectionDefinition::new("posts");
        posts.soft_delete = true;
        posts.fields = vec![
            FieldDefinition::builder("image", FieldType::Relationship)
                .relationship(RelationshipConfig::new("media", false))
                .build(),
        ];
        (media, posts)
    }

    fn insert_referencing_post(conn: &dyn DbConnection) {
        conn.execute(
            "INSERT INTO media (id) VALUES (?1)",
            &[DbValue::Text("m1".into())],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO posts (id, image) VALUES (?1, ?2)",
            &[DbValue::Text("p1".into()), DbValue::Text("m1".into())],
        )
        .unwrap();
        query::ref_count::after_create(
            conn,
            "posts",
            "p1",
            &[FieldDefinition::builder("image", FieldType::Relationship)
                .relationship(RelationshipConfig::new("media", false))
                .build()],
            &LocaleConfig::default(),
        )
        .unwrap();
    }

    /// Move `id` of `table` into the trash.
    fn trash(conn: &dyn DbConnection, table: &str, id: &str) {
        conn.execute(
            &format!("UPDATE {table} SET _deleted_at = '2026-01-01T00:00:00.000Z' WHERE id = ?1"),
            &[DbValue::Text(id.into())],
        )
        .unwrap();
    }

    fn ref_count(conn: &dyn DbConnection, table: &str, id: &str) -> Option<i64> {
        query::ref_count::get_ref_count(conn, table, id).unwrap()
    }

    /// Regression: the CLI purge deleted a document without cancelling its
    /// queued image conversions, which then ran against a missing row.
    #[test]
    fn purge_cancels_queued_image_conversions() {
        let mut media = CollectionDefinition::new("media");
        media.soft_delete = true;
        media.upload = Some(CollectionUpload {
            enabled: true,
            ..Default::default()
        });
        let (_tmp, pool, _registry) = setup_db(&[media.clone()]);
        let conn = pool.get().unwrap();
        conn.execute("INSERT INTO media (id) VALUES ('m1')", &[])
            .unwrap();
        trash(&conn, "media", "m1");
        upload::queue_image_conversion(
            &conn,
            &upload::ImageConvertJobData {
                collection: "media".to_string(),
                document_id: "m1".to_string(),
                source_path: "a.png".to_string(),
                target_path: "a.webp".to_string(),
                format: "webp".to_string(),
                quality: 80,
                url_column: "thumbnail_webp_url".to_string(),
                url_value: "/uploads/a.webp".to_string(),
            },
            1,
        )
        .unwrap();

        let mut purged = Purged::new(None);
        purged
            .purge(
                &conn,
                &media,
                &[TrashedDoc::new("m1", None)],
                &LocaleConfig::default(),
            )
            .unwrap();

        let pending = query::jobs::count_job_runs(
            &conn,
            Some(upload::SYSTEM_IMAGE_CONVERT_JOB),
            Some(JobStatus::Pending),
        )
        .unwrap();
        assert_eq!(pending, 0);
    }

    /// Regression: purging a trashed document must decrement the ref counts
    /// of the documents it references — the raw-delete path used to skip
    /// `before_hard_delete`, leaving targets with inflated `_ref_count`.
    #[test]
    fn purge_decrements_referenced_targets() {
        let (media, posts) = defs_with_relationship();
        let posts_def = posts.clone();
        let (_tmp, db_pool, _) = setup_db(&[media, posts]);

        let mut conn = db_pool.get().unwrap();
        insert_referencing_post(&conn);
        trash(&conn, "posts", "p1");
        assert_eq!(ref_count(&conn, "media", "m1"), Some(1));

        let tx = conn.transaction_immediate().unwrap();
        let mut purged = Purged::new(None);
        purged
            .purge(
                &tx,
                &posts_def,
                &[TrashedDoc::new("p1", None)],
                &LocaleConfig::default(),
            )
            .unwrap();
        tx.commit().unwrap();

        assert_eq!((purged.tally.purged, purged.tally.referenced), (1, 0));
        let conn = db_pool.get().unwrap();
        assert_eq!(ref_count(&conn, "media", "m1"), Some(0));
        assert_eq!(ref_count(&conn, "posts", "p1"), None, "p1 must be gone");
    }

    /// Regression: purging must skip documents that are still referenced by
    /// others — the raw-delete path used to bypass delete protection.
    #[test]
    fn purge_skips_still_referenced_documents() {
        let (media, posts) = defs_with_relationship();
        let media_def = media.clone();
        let (_tmp, db_pool, _) = setup_db(&[media, posts]);

        let mut conn = db_pool.get().unwrap();
        insert_referencing_post(&conn);
        trash(&conn, "media", "m1");

        let tx = conn.transaction_immediate().unwrap();
        let mut purged = Purged::new(None);
        purged
            .purge(
                &tx,
                &media_def,
                &[TrashedDoc::new("m1", None)],
                &LocaleConfig::default(),
            )
            .unwrap();
        tx.commit().unwrap();

        assert_eq!((purged.tally.purged, purged.tally.referenced), (0, 1));
        let conn = db_pool.get().unwrap();
        assert_eq!(
            ref_count(&conn, "media", "m1"),
            Some(1),
            "still-referenced m1 must survive the purge"
        );
    }

    /// Regression: the CLI purge read its candidates before its transaction
    /// and deleted each by id alone, so a document restored in between (the
    /// admin trash view runs beside the CLI) was hard-deleted live. The purge
    /// now re-checks the trash state under the row's lock, as the retention
    /// purge does.
    #[test]
    fn purge_leaves_a_document_restored_after_the_candidate_read() {
        let mut posts = CollectionDefinition::new("posts");
        posts.soft_delete = true;
        let (_tmp, db_pool, _) = setup_db(&[posts.clone()]);

        let mut conn = db_pool.get().unwrap();
        conn.execute("INSERT INTO posts (id) VALUES ('p1')", &[])
            .unwrap();
        trash(&conn, "posts", "p1");

        let ids = find_purge_candidates(&conn, "posts", None).unwrap();
        assert_eq!(ids, vec!["p1".to_string()]);

        // The restore commits between the candidate read and the purge.
        conn.execute("UPDATE posts SET _deleted_at = NULL WHERE id = 'p1'", &[])
            .unwrap();

        let docs: Vec<TrashedDoc<'_>> = ids.iter().map(|id| TrashedDoc::new(id, None)).collect();
        let tx = conn.transaction_immediate().unwrap();
        let mut purged = Purged::new(None);
        purged
            .purge(&tx, &posts, &docs, &LocaleConfig::default())
            .unwrap();
        tx.commit().unwrap();

        assert_eq!((purged.tally.purged, purged.tally.gone), (0, 1));
        let conn = db_pool.get().unwrap();
        assert_eq!(
            ref_count(&conn, "posts", "p1"),
            Some(0),
            "the restored document must survive the purge"
        );
    }

    /// A document trashed again after the candidate read — more recently than
    /// `--older-than` allows — is not purged either.
    #[test]
    fn purge_rechecks_the_age_threshold() {
        let mut posts = CollectionDefinition::new("posts");
        posts.soft_delete = true;
        let (_tmp, db_pool, _) = setup_db(&[posts.clone()]);

        let conn = db_pool.get().unwrap();
        conn.execute(
            "INSERT INTO posts (id, _deleted_at) VALUES \
             ('p1', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
            &[],
        )
        .unwrap();

        let week = Some(7 * 86_400);
        let mut purged = Purged::new(None);
        purged
            .purge(
                &conn,
                &posts,
                &[TrashedDoc::new("p1", week)],
                &LocaleConfig::default(),
            )
            .unwrap();

        assert_eq!((purged.tally.purged, purged.tally.gone), (0, 1));
        assert_eq!(ref_count(&conn, "posts", "p1"), Some(0));
    }

    /// Regression: purge deleted a trashed upload's files inside its
    /// transaction, so a purge that failed on a later document rolled the rows
    /// back while their files were already gone. The rows' files are now only
    /// collected in the transaction and deleted once it commits.
    #[test]
    fn purge_keeps_upload_files_until_the_purge_commits() {
        let mut media = CollectionDefinition::new("media");
        media.soft_delete = true;
        media.upload = Some(CollectionUpload::new());
        media.fields = vec![
            FieldDefinition::builder("filename", FieldType::Text).build(),
            FieldDefinition::builder("url", FieldType::Text).build(),
        ];
        let media_def = media.clone();
        let (tmp, db_pool, _) = setup_db(&[media]);

        let file = tmp.path().join("uploads").join("media").join("a.png");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, b"x").unwrap();

        let mut conn = db_pool.get().unwrap();
        conn.execute(
            "INSERT INTO media (id, filename, url, _deleted_at) VALUES \
             ('m1', 'a.png', '/uploads/media/a.png', '2026-01-01T00:00:00.000Z')",
            &[],
        )
        .unwrap();

        let tx = conn.transaction_immediate().unwrap();
        let mut purged = Purged::new(None);
        purged
            .purge(
                &tx,
                &media_def,
                &[TrashedDoc::new("m1", None)],
                &LocaleConfig::default(),
            )
            .unwrap();
        drop(tx);

        assert!(file.exists(), "a purge that doesn't commit keeps the file");
        assert_eq!(purged.upload_keys, vec!["media/a.png".to_string()]);

        let storage = upload::create_storage(tmp.path(), &CrapConfig::default().upload).unwrap();
        delete_purged_files(&*storage, &purged);
        assert!(!file.exists(), "the collected keys name the file to delete");
    }

    /// Regression: the CLI purge hard-deleted trashed documents without a
    /// delete event, so a trash-view subscriber (over Redis, on `serve`) never
    /// learned they were gone. With a transport to publish on, each purged
    /// row's event is captured — gated by the trash — for after the commit.
    #[test]
    fn purge_captures_each_purged_rows_delete_event() {
        let mut posts = CollectionDefinition::new("posts");
        posts.soft_delete = true;
        let (_tmp, db_pool, _) = setup_db(&[posts.clone()]);

        let conn = db_pool.get().unwrap();
        conn.execute(
            "INSERT INTO posts (id, _deleted_at) VALUES ('p1', '2026-01-01T00:00:00.000Z')",
            &[],
        )
        .unwrap();

        let mut purged = Purged::with_events(PurgeEvents::new(true));
        purged
            .purge(
                &conn,
                &posts,
                &[TrashedDoc::new("p1", None)],
                &LocaleConfig::default(),
            )
            .unwrap();

        let captured = purged.events.captured();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].0, "p1");
        assert!(captured[0].1.trashed, "a purged row is gated by the trash");
    }

    /// Without a transport the purge reads nothing for events.
    #[test]
    fn purge_without_a_transport_captures_nothing() {
        let mut posts = CollectionDefinition::new("posts");
        posts.soft_delete = true;
        let (_tmp, db_pool, _) = setup_db(&[posts.clone()]);

        let conn = db_pool.get().unwrap();
        conn.execute(
            "INSERT INTO posts (id, _deleted_at) VALUES ('p1', '2026-01-01T00:00:00.000Z')",
            &[],
        )
        .unwrap();

        let mut purged = Purged::new(None);
        purged
            .purge(
                &conn,
                &posts,
                &[TrashedDoc::new("p1", None)],
                &LocaleConfig::default(),
            )
            .unwrap();

        assert!(purged.events.captured().is_empty());
    }
}
