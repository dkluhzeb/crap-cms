//! Fixtures and helpers shared by the upload service tests.

use std::{collections::HashMap, sync::Arc};

use anyhow::Result as AnyResult;
use image::{ExtendedColorType, ImageBuffer, ImageEncoder, Rgba, codecs::png::PngEncoder};
use serde_json::Value;
use tempfile::TempDir;

use crate::service::upload::*;
use crate::{
    admin::test_support::test_infra_with_events,
    config::LocaleConfig,
    core::{
        FieldDefinition, FieldType, VersionsConfig,
        cache::CacheBackend,
        upload::{
            CollectionUpload, FALLBACK_MAX_ATTEMPTS, FormatQuality, ImageSizeBuilder,
            key_from_served_url,
        },
    },
    db::{LocaleContext, query},
    service::{AppInfra, OpDeadline, UpdateManyOptions, update_many},
};

pub(super) const MAX_FILE_SIZE: u64 = 1024 * 1024;

/// A cache whose clear panics — standing in for any failure in the work
/// the write envelope runs after its commit (the cache clear is the first
/// of those steps).
struct PanickingCache;

impl CacheBackend for PanickingCache {
    fn get(&self, _key: &str) -> AnyResult<Option<Vec<u8>>> {
        Ok(None)
    }

    fn set(&self, _key: &str, _value: &[u8]) -> AnyResult<()> {
        Ok(())
    }

    fn delete(&self, _key: &str) -> AnyResult<()> {
        Ok(())
    }

    fn clear(&self) -> AnyResult<()> {
        panic!("post-commit cache clear failed")
    }

    fn has(&self, _key: &str) -> AnyResult<bool> {
        Ok(false)
    }

    fn kind(&self) -> &'static str {
        "panicking"
    }
}

/// The test infra over `def`, with a cache whose post-commit clear panics.
pub(super) fn infra_with_post_commit_panic(def: CollectionDefinition) -> (TempDir, Arc<AppInfra>) {
    let (tmp, infra, _rx) = test_infra_with_events(def);

    let Ok(mut infra) = Arc::try_unwrap(infra) else {
        panic!("the test infra must have a single owner");
    };
    infra.cache = Arc::new(PanickingCache);

    (tmp, Arc::new(infra))
}

/// A versioned, draft-enabled `media` upload collection.
pub(super) fn media_with_drafts() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("media");
    def.upload = Some(CollectionUpload {
        enabled: true,
        ..Default::default()
    });
    def.versions = Some(VersionsConfig::new(true, 10));
    def.fields = vec![
        FieldDefinition::builder("filename", FieldType::Text).build(),
        FieldDefinition::builder("mime_type", FieldType::Text).build(),
        FieldDefinition::builder("filesize", FieldType::Number).build(),
        FieldDefinition::builder("url", FieldType::Text).build(),
    ];

    def
}

/// The same collection with a `thumbnail` size whose webp variant is
/// converted on the background queue, and the columns the upload pipeline
/// fills for it (the Lua parser injects these on a real collection).
pub(super) fn media_with_queued_webp() -> CollectionDefinition {
    let mut def = media_with_drafts();

    let upload = def.upload.as_mut().expect("upload");
    upload.image_sizes = vec![
        ImageSizeBuilder::new("thumbnail")
            .width(10)
            .height(10)
            .build(),
    ];
    upload.format_options.webp = Some(FormatQuality::new(80, true));

    let text = |name: &str| FieldDefinition::builder(name, FieldType::Text).build();
    let number = |name: &str| FieldDefinition::builder(name, FieldType::Number).build();
    def.fields.extend([
        number("width"),
        number("height"),
        text("thumbnail_url"),
        number("thumbnail_width"),
        number("thumbnail_height"),
        text("thumbnail_webp_url"),
    ]);

    def
}

pub(super) fn infra() -> (TempDir, Arc<AppInfra>, CollectionDefinition) {
    infra_for(media_with_drafts())
}

pub(super) fn infra_for(
    def: CollectionDefinition,
) -> (TempDir, Arc<AppInfra>, CollectionDefinition) {
    let (tmp, infra, _rx) = test_infra_with_events(def.clone());

    (tmp, infra, def)
}

/// A small in-memory PNG, so the upload pipeline produces real sizes.
pub(super) fn png(width: u32, height: u32) -> Vec<u8> {
    let img: ImageBuffer<Rgba<u8>, Vec<u8>> = ImageBuffer::from_fn(width, height, |x, y| {
        Rgba([
            u8::try_from(x % 256).unwrap_or(0),
            u8::try_from(y % 256).unwrap_or(0),
            128,
            255,
        ])
    });

    let mut buf = Vec::new();
    PngEncoder::new(&mut buf)
        .write_image(img.as_raw(), width, height, ExtendedColorType::Rgba8)
        .expect("encode PNG");

    buf
}

pub(super) fn image_file(name: &str, size: u32) -> UploadedFile {
    UploadedFile {
        filename: name.to_string(),
        content_type: "image/png".to_string(),
        data: png(size, size),
    }
}

/// The payloads of the image-conversion jobs still queued.
pub(super) fn queued_conversion_payloads(infra: &Arc<AppInfra>) -> Vec<String> {
    let conn = infra.pool.get().expect("connection");

    query::jobs::list_job_runs(&conn, None, None, 100, 0)
        .expect("list job runs")
        .into_iter()
        .map(|run| run.data)
        .collect()
}

/// The latest version snapshot of a document.
pub(super) fn latest_snapshot(infra: &Arc<AppInfra>, id: &str) -> Value {
    let conn = infra.pool.get().expect("connection");

    query::find_latest_version(&conn, "media", id)
        .expect("find latest version")
        .expect("a version")
        .snapshot
}

/// The published row's `url`.
pub(super) fn live_url(infra: &Arc<AppInfra>, def: &CollectionDefinition, id: &str) -> String {
    let conn = infra.pool.get().expect("connection");

    query::find_by_id(&conn, "media", def, id, None)
        .expect("find")
        .expect("the published row")
        .get_str("url")
        .expect("a url")
        .to_string()
}

pub(super) fn file(name: &str, body: &[u8]) -> UploadedFile {
    UploadedFile {
        filename: name.to_string(),
        content_type: "text/plain".to_string(),
        data: body.to_vec(),
    }
}

pub(super) fn empty_form(def: &CollectionDefinition) -> FormData {
    FormData::from_raw(HashMap::new(), &def.fields)
}

/// The storage key the document's `url` column points at.
pub(super) fn stored_key(doc: &Document) -> String {
    key_from_served_url(doc.get_str("url").expect("a url"))
        .expect("a served url")
        .to_string()
}

pub(super) fn create(
    infra: &Arc<AppInfra>,
    def: &CollectionDefinition,
    f: &UploadedFile,
) -> Document {
    let ctx = ServiceContext::collection("media", def)
        .infra(infra)
        .build();

    create_upload(
        &ctx,
        CreateUploadInput {
            storage: &infra.storage,
            file: f,
            form: empty_form(def),
            locale_ctx: None,
            password: None,
            ui_locale: None,
            draft: false,
            upload_max_file_size: MAX_FILE_SIZE,
            image_max_attempts: FALLBACK_MAX_ATTEMPTS,
        },
    )
    .expect("create upload")
    .doc
}

pub(super) fn update(
    infra: &Arc<AppInfra>,
    def: &CollectionDefinition,
    id: &str,
    f: Option<UploadedFile>,
    draft: bool,
) -> Document {
    update_in_locale(infra, def, id, f, draft, None)
}

pub(super) fn update_in_locale(
    infra: &Arc<AppInfra>,
    def: &CollectionDefinition,
    id: &str,
    f: Option<UploadedFile>,
    draft: bool,
    locale_ctx: Option<&LocaleContext>,
) -> Document {
    let ctx = ServiceContext::collection("media", def)
        .infra(infra)
        .build();

    update_upload(
        &ctx,
        UpdateUploadInput {
            id,
            storage: &infra.storage,
            file: f,
            form: empty_form(def),
            locale_ctx,
            password: None,
            ui_locale: None,
            draft,
            upload_max_file_size: MAX_FILE_SIZE,
            image_max_attempts: FALLBACK_MAX_ATTEMPTS,
            form_echoes_locked_fields: false,
        },
    )
    .expect("update upload")
    .doc
}

/// The source key of the conversion job for this document's thumbnail —
/// the size file beside the original, under the same stem.
pub(super) fn thumbnail_source(doc: &Document) -> String {
    let key = stored_key(doc);
    let (stem, ext) = key.rsplit_once('.').expect("an extension");

    format!("{stem}_thumbnail.{ext}")
}

/// `media_with_queued_webp` capped at one version, so a publish's own
/// snapshot prunes every earlier one.
pub(super) fn media_with_queued_webp_capped() -> CollectionDefinition {
    let mut def = media_with_queued_webp();
    def.versions = Some(VersionsConfig::new(true, 1));

    def
}

/// Run a bulk publish over every published document of the collection.
pub(super) fn bulk_publish(infra: &Arc<AppInfra>, def: &CollectionDefinition) {
    let ctx = ServiceContext::collection("media", def)
        .infra(infra)
        .build();

    update_many(
        &ctx,
        &[],
        &DocumentFields::new(),
        &LocaleConfig::default(),
        &UpdateManyOptions {
            locale_ctx: None,
            run_hooks: false,
            draft: false,
            ui_locale: None,
            max_documents: 0,
            deadline: OpDeadline::none(),
        },
    )
    .expect("bulk publish");
}

/// Whether any regular file lives under `dir`, at any depth.
pub(super) fn has_file_under(dir: &std::path::Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };

    entries.flatten().any(|entry| {
        let path = entry.path();

        path.is_file() || (path.is_dir() && has_file_under(&path))
    })
}
