//! What restoring a version would no longer find: relation targets that are
//! gone and files the storage backend no longer holds.

use serde::Serialize;
use serde_json::Value;

use crate::{
    config::LocaleConfig,
    core::{Document, Registry, document::VersionSnapshot, upload::snapshot_file_keys},
    db::{
        LocaleContext, ops,
        query::{self, MissingRelation},
    },
    service::{
        ReadStripArgs, ServiceContext, ServiceError, find_stored_version,
        helpers::strip_unreadable, read_version_snapshot,
    },
};

/// What restoring one version would not find any more.
///
/// Both halves are warnings, never blocks: the restore still runs, and the
/// confirmation page says what it would leave dangling.
#[derive(Debug, Clone, Default, Serialize)]
pub struct VersionGaps {
    /// Relations the restore would write whose target documents are gone.
    pub relations: Vec<MissingRelation>,
    /// Storage keys the snapshot names whose bytes the backend no longer
    /// holds — a file a later write replaced and the cleanup removed.
    pub files: Vec<String>,
}

/// A version as the viewer reads it, plus what its restore would no longer
/// find.
///
/// The relation check inspects what a restore writes — the stored snapshot,
/// every locale it records — read the way the viewer reads it: one view per
/// locale, holding that locale's own values, with the read-denied and hidden
/// fields stripped as a version read strips them. A value the viewer cannot
/// read is never reported. `None` when the version does not exist or is hidden
/// from the viewer.
///
/// # Errors
///
/// Returns the version read's errors (access denied, hook errors), or a
/// backend error if the stored snapshot cannot be read.
pub fn version_restore_gaps(
    ctx: &ServiceContext,
    registry: &Registry,
    version_id: &str,
) -> Result<Option<(VersionSnapshot, VersionGaps)>, ServiceError> {
    // The gated read row, still carrying the STORED snapshot: the views and the
    // file keys below are what a restore would write, so both must be taken
    // before the read shaping rewrites the snapshot into the document a read
    // returns.
    let Some(mut version) = find_stored_version(ctx, version_id)? else {
        return Ok(None);
    };

    let views = readable_locale_views(ctx, &version)?;
    let files = missing_snapshot_files(ctx, &version.snapshot);

    // The per-locale `views` above already cover every locale the restore
    // would write; the returned snapshot is the default-locale read shape.
    read_version_snapshot(ctx, ctx.read_hooks()?, &mut version, None)?;

    let conn = ctx.resolve_conn()?;
    let relations = query::find_missing_relations(conn.as_ref(), registry, &views, ctx.fields()?);

    Ok(Some((version, VersionGaps { relations, files })))
}

/// The files a restore of this snapshot would leave dangling: the storage keys
/// it names whose bytes the backend no longer holds.
///
/// The file columns are server-derived and not localized, so they are read from
/// the snapshot as stored rather than per locale. Nothing to report without an
/// upload collection or without a storage backend to ask.
///
/// A backend that cannot answer for a key reports it present: existence is a
/// membership question, and a transient backend failure must not turn into a
/// confident "this file is gone" on the confirmation page.
fn missing_snapshot_files(ctx: &ServiceContext, snapshot: &Value) -> Vec<String> {
    let Some(storage) = ctx.storage.as_ref() else {
        return Vec::new();
    };

    let Some(upload) = ctx
        .collection_def()
        .ok()
        .and_then(|def| def.upload.as_ref())
        .filter(|upload| upload.enabled)
    else {
        return Vec::new();
    };

    let mut missing: Vec<String> = snapshot_file_keys(snapshot, upload)
        .into_iter()
        .filter(|key| !storage.exists(key).unwrap_or(true))
        .collect();

    // A size url and its format variants can name the same key; report each
    // missing file once, in a stable order.
    missing.sort();
    missing.dedup();

    missing
}

/// Each locale's view of a stored snapshot as the viewer reads it: resolved for
/// that locale alone, then stripped of the fields a read withholds —
/// read-denied ones judged in that locale, and hidden ones.
fn readable_locale_views(
    ctx: &ServiceContext,
    stored: &VersionSnapshot,
) -> Result<Vec<Document>, ServiceError> {
    let hooks = ctx.read_hooks()?;
    let fields = ctx.fields()?;
    let parent = stored.parent.to_string();

    let mut views = Vec::new();

    for locale_ctx in exact_locale_contexts(ctx.locale_config) {
        let Some(mut view) =
            ops::snapshot_read_document(&parent, &stored.snapshot, fields, locale_ctx.as_ref())?
        else {
            continue;
        };

        let locale = locale_ctx.as_ref().map(LocaleContext::access_locale);
        strip_unreadable(
            hooks,
            &ReadStripArgs::builder(fields, ctx.slug)
                .user(ctx.user)
                .locale(locale)
                .build(),
            &mut view,
        );

        views.push(view);
    }

    Ok(views)
}

/// One exact context per configured locale: a restore writes each locale's own
/// value, never its fallback's. A single `None` without localization.
fn exact_locale_contexts(config: Option<&LocaleConfig>) -> Vec<Option<LocaleContext>> {
    let Some(config) = config.filter(|c| c.is_enabled()) else {
        return vec![None];
    };

    config
        .locales
        .iter()
        .map(|locale| Some(LocaleContext::exact(config, locale)))
        .collect()
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use std::{collections::HashMap, fs, sync::Arc};

    use anyhow::Result;
    use serde_json::json;

    use super::*;
    use crate::{
        config::{CrapConfig, DatabaseConfig},
        core::{
            CollectionDefinition, FieldDefinition, FieldType, HookRef, RelationshipConfig,
            ReqContext, SharedStorage,
            collection::{Hooks, VersionsConfig},
            upload::{CollectionUpload, ImageSizeBuilder, storage::LocalStorage},
        },
        db::{AccessResult, DbConnection, DbPool, migrate, pool},
        hooks::{AccessCheckInput, lifecycle::AfterReadCtx},
        service::{FieldReadStrip, ReadHooks, document_info::test_support::DenyMarkedFields},
    };

    /// Locales `en` (default) and `de`.
    fn en_de() -> LocaleConfig {
        LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: true,
        }
    }

    /// An upload field into `media`.
    fn media_upload(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Upload)
            .relationship(RelationshipConfig::new("media", false))
            .build()
    }

    /// A versioned `posts` collection with `fields`, synced under `en_de`
    /// beside `authors`, `tags` and `media`; post `p1` plus one existing
    /// target in each (`a1`, `t1`, `m1`).
    fn localized_posts(fields: Vec<FieldDefinition>) -> (tempfile::TempDir, DbPool, Registry) {
        let mut posts = CollectionDefinition::new("posts");
        posts.versions = Some(VersionsConfig::new(false, 0));
        posts.fields = fields;

        let tmp = tempfile::tempdir().unwrap();
        let config = CrapConfig {
            database: DatabaseConfig {
                path: "test.db".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };
        let db_pool = pool::create_pool(tmp.path(), &config).unwrap();

        let shared = Registry::shared();
        {
            let mut reg = shared.write().unwrap();
            for slug in ["authors", "tags", "media"] {
                reg.register_collection(CollectionDefinition::new(slug));
            }
            reg.register_collection(posts);
        }
        let registry = (*Registry::snapshot(&shared)).clone();
        migrate::sync_all(&db_pool, &registry, &en_de()).unwrap();

        let conn = db_pool.get().unwrap();
        for (table, id) in [
            ("posts", "p1"),
            ("authors", "a1"),
            ("tags", "t1"),
            ("media", "m1"),
        ] {
            conn.execute(
                &format!("INSERT INTO \"{table}\" (id) VALUES ('{id}')"),
                &[],
            )
            .unwrap();
        }
        drop(conn);

        (tmp, db_pool, registry)
    }

    /// The missing relations a viewer with `hooks` sees on a version of `p1`
    /// stored as `snapshot`.
    fn missing_for(
        db_pool: &DbPool,
        registry: &Registry,
        hooks: &dyn ReadHooks,
        snapshot: &Value,
    ) -> Vec<MissingRelation> {
        let conn = db_pool.get().unwrap();
        let version = query::create_version(&conn, "posts", "p1", "published", snapshot).unwrap();
        let def = registry.get_collection("posts").unwrap();
        let locale = en_de();
        let ctx = ServiceContext::collection("posts", def)
            .conn(&conn)
            .read_hooks(hooks)
            .locale_config(Some(&locale))
            .build();

        let (_, gaps) = version_restore_gaps(&ctx, registry, &version.id)
            .unwrap()
            .expect("the version is visible");

        gaps.relations
    }

    /// Each reported field's sorted missing ids and total id count.
    fn by_field(missing: &[MissingRelation]) -> HashMap<&str, (Vec<&str>, usize)> {
        missing
            .iter()
            .map(|m| {
                let mut ids: Vec<&str> = m.missing_ids.iter().map(String::as_str).collect();
                ids.sort_unstable();

                (m.field_name.as_str(), (ids, m.total_ids))
            })
            .collect()
    }

    /// A stored snapshot records a localized reference once per locale
    /// (`author__de`, `tags__de`, `slides__de`, `meta__hero__de`) and a restore
    /// writes every locale back. The scan read only the bare key, so a target
    /// missing in a non-default locale went unreported. Each field is reported
    /// once, its ids combined across locales.
    #[test]
    fn targets_missing_in_a_non_default_locale_are_reported() {
        let mut hero = media_upload("hero");
        hero.localized = true;

        let (_tmp, db_pool, registry) = localized_posts(vec![
            FieldDefinition::builder("author", FieldType::Relationship)
                .relationship(RelationshipConfig::new("authors", false))
                .localized(true)
                .build(),
            FieldDefinition::builder("tags", FieldType::Relationship)
                .relationship(RelationshipConfig::new("tags", true))
                .localized(true)
                .build(),
            FieldDefinition::builder("slides", FieldType::Array)
                .localized(true)
                .fields(vec![media_upload("image")])
                .build(),
            FieldDefinition::builder("meta", FieldType::Group)
                .fields(vec![hero])
                .build(),
        ]);

        let snapshot = json!({
            "author": "a1", "author__en": "a1", "author__de": "a_gone",
            "tags": ["t1"], "tags__en": ["t1", "t_gone"], "tags__de": ["t_gone"],
            "slides": [{ "image": "m1" }],
            "slides__en": [{ "image": "m1" }],
            "slides__de": [{ "image": "m_gone" }],
            "meta": { "hero": "m1" }, "meta__hero__en": "m1", "meta__hero__de": "m_gone"
        });
        let missing = missing_for(&db_pool, &registry, &NoStripHooks, &snapshot);

        assert_eq!(
            by_field(&missing),
            HashMap::from([
                ("author", (vec!["a_gone"], 2)),
                ("tags", (vec!["t_gone"], 2)),
                ("slides.image", (vec!["m_gone"], 2)),
                ("meta.hero", (vec!["m_gone"], 2)),
            ]),
            "{missing:?}"
        );
    }

    /// The restore-confirm page listed missing relations for every field of the
    /// stored snapshot, so a viewer learned the names and referenced ids of
    /// fields they may not read. A read-denied or hidden field stays
    /// unreported in every locale.
    #[test]
    fn fields_the_viewer_cannot_read_are_not_reported_in_any_locale() {
        let mut private_image = media_upload("private_image");
        private_image.localized = true;
        private_image.access.read = Some(HookRef::new("deny"));

        let mut internal_image = media_upload("internal_image");
        internal_image.localized = true;
        internal_image.hidden = true;

        let (_tmp, db_pool, registry) =
            localized_posts(vec![media_upload("image"), private_image, internal_image]);

        let snapshot = json!({
            "image": "m_gone",
            "private_image": "m1",
            "private_image__en": "m1",
            "private_image__de": "m_private_gone",
            "internal_image": "m1",
            "internal_image__en": "m1",
            "internal_image__de": "m_internal_gone"
        });
        let missing = missing_for(&db_pool, &registry, &DenyMarkedFields, &snapshot);

        let names: Vec<&str> = missing.iter().map(|m| m.field_name.as_str()).collect();
        assert_eq!(names, vec!["image"], "{missing:?}");
    }

    /// A restore rewrites the snapshot's file urls onto the row, so the
    /// confirmation has to say which of those files storage no longer holds.
    /// Every url column the snapshot carries is checked — the main file and
    /// each size — and a file that is still there is not reported.
    #[test]
    fn a_version_naming_a_deleted_file_reports_it_as_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let uploads = tmp.path().join("uploads").join("media");
        fs::create_dir_all(&uploads).unwrap();
        fs::write(uploads.join("here.png"), b"bytes").unwrap();

        let storage: SharedStorage = Arc::new(LocalStorage::new(tmp.path().join("uploads")));

        let mut upload = CollectionUpload::new();
        upload.image_sizes = vec![
            ImageSizeBuilder::new("thumbnail")
                .width(300)
                .height(300)
                .build(),
        ];

        let mut media = CollectionDefinition::new("media");
        media.upload = Some(upload);
        media.versions = Some(VersionsConfig::new(false, 0));

        let config = CrapConfig {
            database: DatabaseConfig {
                path: "test.db".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };
        let db_pool = pool::create_pool(tmp.path(), &config).unwrap();

        let shared = Registry::shared();
        shared.write().unwrap().register_collection(media);
        let registry = (*Registry::snapshot(&shared)).clone();
        migrate::sync_all(&db_pool, &registry, &LocaleConfig::default()).unwrap();

        let conn = db_pool.get().unwrap();
        conn.execute("INSERT INTO \"media\" (id) VALUES ('m1')", &[])
            .unwrap();

        let snapshot = json!({
            "url": "/uploads/media/gone.png",
            "filename": "gone.png",
            "thumbnail_url": "/uploads/media/here.png",
        });
        let version = query::create_version(&conn, "media", "m1", "published", &snapshot).unwrap();

        let def = registry.get_collection("media").unwrap();
        let hooks = NoStripHooks;
        let ctx = ServiceContext::collection("media", def)
            .conn(&conn)
            .read_hooks(&hooks)
            .storage(Some(storage))
            .build();

        let (_, gaps) = version_restore_gaps(&ctx, &registry, &version.id)
            .unwrap()
            .expect("the version is visible");

        assert_eq!(gaps.files, vec!["media/gone.png".to_string()], "{gaps:?}");
    }

    /// Without a storage backend there is nothing to ask, so the report stays
    /// empty rather than claiming every file is gone.
    #[test]
    fn a_context_without_storage_reports_no_missing_files() {
        let mut media = CollectionDefinition::new("media");
        media.upload = Some(CollectionUpload::new());

        let ctx = ServiceContext::collection("media", &media).build();
        let snapshot = json!({ "url": "/uploads/media/gone.png" });

        assert!(missing_snapshot_files(&ctx, &snapshot).is_empty());
    }

    /// Read hooks that allow every access check and strip nothing.
    struct NoStripHooks;

    impl ReadHooks for NoStripHooks {
        fn before_read(&self, _: &Hooks, _: &str, _: &str, _: Option<&str>) -> Result<ReqContext> {
            Ok(ReqContext::new())
        }

        fn after_read_one(&self, _: &AfterReadCtx, doc: Document) -> Document {
            doc
        }

        fn check_access(&self, _: &AccessCheckInput<'_>) -> Result<AccessResult> {
            Ok(AccessResult::Allowed)
        }
    }

    impl FieldReadStrip for NoStripHooks {}
}
