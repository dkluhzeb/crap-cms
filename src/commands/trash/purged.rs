//! What a CLI purge of the trash did: the documents it skipped, the files it
//! leaves for after the commit, and the delete events it owes.

use anyhow::Result;

use crate::{
    cli,
    config::LocaleConfig,
    core::{CollectionDefinition, upload, upload::StorageBackend},
    db::{DbConnection, query},
    service::{AppInfra, PurgeEvents, owned_file_keys},
};

/// Delete the files of the uploads a committed purge removed.
pub(super) fn delete_purged_files(storage: &dyn StorageBackend, purged: &Purged) {
    upload::delete_storage_keys(storage, &purged.upload_keys);
}

/// What purging documents did, and the delete events it owes once it has
/// committed.
pub(super) struct Purged {
    /// Documents skipped because others still reference them.
    pub(super) skipped: u64,
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

        Self {
            skipped: 0,
            upload_keys: Vec::new(),
            events: PurgeEvents::new(capture),
        }
    }

    /// Permanently delete a list of documents, cleaning up FTS and reference
    /// counts, and collect the storage keys the purged uploads owned — their
    /// rows' and their version snapshots' — for file cleanup, and each
    /// document's delete event. Documents that are still referenced by others
    /// (`_ref_count > 0`) are skipped — the same delete protection the server
    /// surfaces enforce.
    pub(super) fn purge(
        &mut self,
        tx: &dyn DbConnection,
        (slug, def): (&str, &CollectionDefinition),
        ids: &[String],
        locale: &LocaleConfig,
    ) -> Result<()> {
        // The row lookup needs the locale context: a collection with localized
        // fields has no bare columns to select.
        let locale_ctx = query::LocaleContext::default_for(locale);

        for id in ids {
            if query::ref_count::get_ref_count(tx, slug, id)?.unwrap_or(0) > 0 {
                cli::warning(&format!(
                    "Skipping {slug} / {id} — still referenced by other documents"
                ));
                self.skipped += 1;
                continue;
            }

            // Every file the document owns, its version snapshots' included —
            // collected before the purge removes the rows that name them.
            self.upload_keys
                .extend(owned_file_keys(tx, def, id, locale_ctx.as_ref())?);

            self.events.purge(tx, def, id, locale)?;
        }

        Ok(())
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
        commands::trash::tests::setup_db,
        config::CrapConfig,
        core::{
            JobStatus,
            field::{FieldDefinition, FieldType, RelationshipConfig},
            upload::CollectionUpload,
        },
        db::DbValue,
    };

    fn defs_with_relationship() -> (CollectionDefinition, CollectionDefinition) {
        let media = CollectionDefinition::new("media");
        let mut posts = CollectionDefinition::new("posts");
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
                ("media", &media),
                &["m1".to_string()],
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
        assert_eq!(ref_count(&conn, "media", "m1"), Some(1));

        let tx = conn.transaction_immediate().unwrap();
        let mut purged = Purged::new(None);
        purged
            .purge(
                &tx,
                ("posts", &posts_def),
                &["p1".to_string()],
                &LocaleConfig::default(),
            )
            .unwrap();
        tx.commit().unwrap();

        assert_eq!(purged.skipped, 0);
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

        let tx = conn.transaction_immediate().unwrap();
        let mut purged = Purged::new(None);
        purged
            .purge(
                &tx,
                ("media", &media_def),
                &["m1".to_string()],
                &LocaleConfig::default(),
            )
            .unwrap();
        tx.commit().unwrap();

        assert_eq!(purged.skipped, 1);
        let conn = db_pool.get().unwrap();
        assert_eq!(
            ref_count(&conn, "media", "m1"),
            Some(1),
            "still-referenced m1 must survive the purge"
        );
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
                ("media", &media_def),
                &["m1".to_string()],
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

        let mut purged = Purged {
            skipped: 0,
            upload_keys: Vec::new(),
            events: PurgeEvents::new(true),
        };
        purged
            .purge(
                &conn,
                ("posts", &posts),
                &["p1".to_string()],
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
                ("posts", &posts),
                &["p1".to_string()],
                &LocaleConfig::default(),
            )
            .unwrap();

        assert!(purged.events.captured().is_empty());
    }
}
