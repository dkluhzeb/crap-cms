//! Upload service — the ONE entry every upload write goes through.
//!
//! Owns the file half of an upload write: store the file, inject the
//! server-derived metadata, hand the write the conversions the file queued, and
//! release the stored bytes only once the write has returned. Every surface
//! (REST multipart, the admin edit form) reaches a write on an upload
//! collection through here; what happens to the *previous* file — its bytes and
//! the conversions still queued for it — is settled inside the write
//! transaction by `service::write::settle_upload_write`, so the rule is decided
//! once rather than per surface.
//!
//! Surfaces keep what is theirs: multipart parsing, auth, CSRF, and response
//! formatting.

use anyhow::anyhow;

use crate::{
    admin::{FormData, strip_locale_locked_for_publish},
    core::{
        CollectionDefinition, Document, DocumentFields, FieldError, ReqContext, SharedStorage,
        ValidationError,
        upload::{
            CleanupGuard, QueuedConversion, UploadedFile, inject_upload_metadata, process_upload,
        },
    },
    db::LocaleContext,
    service::{
        ServiceContext, UploadConversions, WriteInput, create_document, op::reject_all_locales,
        update_document,
    },
};

use super::ServiceError;

type Result<T> = std::result::Result<T, ServiceError>;

/// Where a file goes and how large it may be.
struct FileStore<'a> {
    storage: &'a SharedStorage,
    max_file_size: u64,
}

impl<'a> FileStore<'a> {
    fn new(storage: &'a SharedStorage, max_file_size: u64) -> Self {
        Self {
            storage,
            max_file_size,
        }
    }
}

/// Drop every caller-supplied server-derived upload column before the real ones
/// are injected, so a forged `url`/`*_url` (including a not-yet-processed
/// queued-format size) can never survive even on this trusted, file-bearing
/// path. On a no-file update their absence means "keep what is stored".
fn strip_derived_columns(form: &mut FormData, def: &CollectionDefinition) {
    let Some(upload) = def.upload.as_ref() else {
        return;
    };

    for name in upload.derived_field_names() {
        form.take(&name);
    }
}

/// Store the file and inject its server-derived metadata into `form`.
///
/// Returns the guard that removes the stored bytes unless the write commits,
/// together with the conversions the file queued.
///
/// # Errors
///
/// A rejected file (type, size, unreadable image) is a `_file` validation
/// error, so every surface can render it against the form's file input.
fn store_file(
    ctx: &ServiceContext,
    store: &FileStore<'_>,
    file: &UploadedFile,
    form: &mut FormData,
) -> Result<(CleanupGuard, Vec<QueuedConversion>)> {
    let def = ctx.collection_def()?;

    let upload_config = def
        .upload
        .clone()
        .ok_or_else(|| ServiceError::Internal(anyhow!("Upload config missing")))?;

    let (processed, guard) = process_upload(
        file,
        &upload_config,
        store.storage,
        ctx.slug,
        store.max_file_size,
    )
    .map_err(|e| {
        ServiceError::Validation(ValidationError::new(vec![FieldError::new(
            "_file",
            e.to_string(),
        )]))
    })?;

    inject_upload_metadata(form.raw_mut(), &processed);

    Ok((guard, processed.queued_conversions))
}

/// Result of a successful upload-create operation.
pub struct UploadCreateResult {
    pub doc: Document,
    pub req_context: ReqContext,
}

/// Result of a successful upload-update operation.
pub struct UploadUpdateResult {
    pub doc: Document,
    pub req_context: ReqContext,
}

/// Input for [`create_upload`].
pub struct CreateUploadInput<'a> {
    pub storage: &'a SharedStorage,
    pub file: &'a UploadedFile,
    pub form: FormData,
    /// The locale the row is written in. A create is always in the default
    /// locale (the write rejects any other), but a localized collection still
    /// needs the context to address `title__en` rather than a bare `title`.
    pub locale_ctx: Option<&'a LocaleContext>,
    pub password: Option<String>,
    pub ui_locale: Option<String>,
    pub draft: bool,
    pub upload_max_file_size: u64,
    /// `max_attempts` the queued conversions are inserted with. Derived from
    /// `JobsConfig::system_image_max_attempts()` at the surface; tests can pass
    /// [`crate::core::upload::FALLBACK_MAX_ATTEMPTS`].
    pub image_max_attempts: u32,
}

/// Input for [`update_upload`].
pub struct UpdateUploadInput<'a> {
    pub id: &'a str,
    pub storage: &'a SharedStorage,
    pub file: Option<UploadedFile>,
    pub form: FormData,
    pub locale_ctx: Option<&'a LocaleContext>,
    pub password: Option<String>,
    pub ui_locale: Option<String>,
    pub draft: bool,
    pub upload_max_file_size: u64,
    /// See [`CreateUploadInput::image_max_attempts`].
    pub image_max_attempts: u32,
    /// The data came from an HTML edit form, which round-trips shared
    /// (non-localized) fields as read-only inputs. Under a non-default locale a
    /// publish then drops them instead of failing the write, exactly as the
    /// draft path already does. A programmatic caller leaves this false: it
    /// spelled such a field out deliberately, so the write still rejects it
    /// rather than silently discarding the value.
    pub form_echoes_locked_fields: bool,
}

/// Process a file and create an upload document.
///
/// # Errors
///
/// Returns a `ValidationError` if the write names the all-locales mode or file
/// processing fails, or any service-layer error from the underlying
/// `create_document` (access denied, validation, …).
pub fn create_upload(
    ctx: &ServiceContext,
    mut input: CreateUploadInput<'_>,
) -> Result<UploadCreateResult> {
    let def = ctx.collection_def()?;

    // A file-bearing write reaches `create_document` without an operation, so
    // the rule every other write gets from the operation body is applied here.
    reject_all_locales(input.locale_ctx)?;

    strip_derived_columns(&mut input.form, def);

    let store = FileStore::new(input.storage, input.upload_max_file_size);
    let (mut guard, conversions) = store_file(ctx, &store, input.file, &mut input.form)?;

    let (doc, req_context) = create_document(
        ctx,
        WriteInput::builder(input.form)
            .password(input.password.as_deref())
            .locale_ctx(input.locale_ctx)
            .draft(input.draft)
            .ui_locale(input.ui_locale)
            .trusted_upload_metadata(true)
            .upload_conversions(Some(UploadConversions::new(
                conversions,
                input.image_max_attempts,
            )))
            .build(),
    )?;

    // The bytes stay only now that the row is written; an error above dropped
    // the guard and took them with it.
    guard.commit();

    Ok(UploadCreateResult { doc, req_context })
}

/// Process a file (optional) and update an upload document.
///
/// # Errors
///
/// Returns a `ValidationError` if the write names the all-locales mode or file
/// processing fails, or any service-layer error from the underlying
/// `update_document` (access denied, validation, …).
pub fn update_upload(
    ctx: &ServiceContext,
    input: UpdateUploadInput<'_>,
) -> Result<UploadUpdateResult> {
    let def = ctx.collection_def()?;

    let UpdateUploadInput {
        id,
        storage,
        file,
        mut form,
        locale_ctx,
        password,
        ui_locale,
        draft,
        upload_max_file_size,
        image_max_attempts,
        form_echoes_locked_fields,
    } = input;

    // See `create_upload`: the same rejection, applied where the write is.
    reject_all_locales(locale_ctx)?;

    strip_derived_columns(&mut form, def);

    let store = FileStore::new(storage, upload_max_file_size);

    let stored = match file.as_ref() {
        Some(file) => Some(store_file(ctx, &store, file, &mut form)?),
        None => None,
    };

    let (guard, conversions) = match stored {
        Some((guard, queued)) => (
            Some(guard),
            Some(UploadConversions::new(queued, image_max_attempts)),
        ),
        None => (None, None),
    };

    let mut data: DocumentFields = form.into();

    if form_echoes_locked_fields {
        data = strip_locale_locked_for_publish(data, &def.fields, locale_ctx, draft);
    }

    let (doc, req_context) = update_document(
        ctx,
        id,
        WriteInput::builder(data)
            .password(password.as_deref())
            .locale_ctx(locale_ctx)
            .draft(draft)
            .ui_locale(ui_locale)
            .trusted_upload_metadata(true)
            .upload_conversions(conversions)
            .build(),
    )?;

    if let Some(mut guard) = guard {
        guard.commit();
    }

    Ok(UploadUpdateResult { doc, req_context })
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use std::{collections::HashMap, sync::Arc};

    use image::{ExtendedColorType, ImageBuffer, ImageEncoder, Rgba, codecs::png::PngEncoder};
    use serde_json::Value;
    use tempfile::TempDir;

    use super::*;
    use crate::{
        admin::test_support::test_infra_with_events,
        config::LocaleConfig,
        core::{
            FieldDefinition, FieldType, VersionsConfig,
            upload::{
                CollectionUpload, FALLBACK_MAX_ATTEMPTS, FormatQuality, ImageSizeBuilder,
                key_from_served_url,
            },
        },
        db::{LocaleMode, query},
        service::{AppInfra, delete_document},
    };

    const MAX_FILE_SIZE: u64 = 1024 * 1024;

    /// A versioned, draft-enabled `media` upload collection.
    fn media_with_drafts() -> CollectionDefinition {
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
    fn media_with_queued_webp() -> CollectionDefinition {
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

    fn infra() -> (TempDir, Arc<AppInfra>, CollectionDefinition) {
        infra_for(media_with_drafts())
    }

    fn infra_for(def: CollectionDefinition) -> (TempDir, Arc<AppInfra>, CollectionDefinition) {
        let (tmp, infra, _rx) = test_infra_with_events(def.clone());

        (tmp, infra, def)
    }

    /// A small in-memory PNG, so the upload pipeline produces real sizes.
    fn png(width: u32, height: u32) -> Vec<u8> {
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

    fn image_file(name: &str, size: u32) -> UploadedFile {
        UploadedFile {
            filename: name.to_string(),
            content_type: "image/png".to_string(),
            data: png(size, size),
        }
    }

    /// The payloads of the image-conversion jobs still queued.
    fn queued_conversion_payloads(infra: &Arc<AppInfra>) -> Vec<String> {
        let conn = infra.pool.get().expect("connection");

        query::jobs::list_job_runs(&conn, None, None, 100, 0)
            .expect("list job runs")
            .into_iter()
            .map(|run| run.data)
            .collect()
    }

    /// The latest version snapshot of a document.
    fn latest_snapshot(infra: &Arc<AppInfra>, id: &str) -> Value {
        let conn = infra.pool.get().expect("connection");

        query::find_latest_version(&conn, "media", id)
            .expect("find latest version")
            .expect("a version")
            .snapshot
    }

    /// The published row's `url`.
    fn live_url(infra: &Arc<AppInfra>, def: &CollectionDefinition, id: &str) -> String {
        let conn = infra.pool.get().expect("connection");

        query::find_by_id(&conn, "media", def, id, None)
            .expect("find")
            .expect("the published row")
            .get_str("url")
            .expect("a url")
            .to_string()
    }

    fn file(name: &str, body: &[u8]) -> UploadedFile {
        UploadedFile {
            filename: name.to_string(),
            content_type: "text/plain".to_string(),
            data: body.to_vec(),
        }
    }

    fn empty_form(def: &CollectionDefinition) -> FormData {
        FormData::from_raw(HashMap::new(), &def.fields)
    }

    /// The storage key the document's `url` column points at.
    fn stored_key(doc: &Document) -> String {
        key_from_served_url(doc.get_str("url").expect("a url"))
            .expect("a served url")
            .to_string()
    }

    fn create(infra: &Arc<AppInfra>, def: &CollectionDefinition, f: &UploadedFile) -> Document {
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

    fn update(
        infra: &Arc<AppInfra>,
        def: &CollectionDefinition,
        id: &str,
        f: Option<UploadedFile>,
        draft: bool,
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
                locale_ctx: None,
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

    /// Regression: a draft save carrying a new file deleted the file the
    /// PUBLISHED row still references. Every live page then 404s and no version
    /// brings the bytes back. The published row and its file must both survive
    /// a draft save untouched.
    #[test]
    fn a_draft_save_with_a_new_file_keeps_the_published_file() {
        let (_tmp, infra, def) = infra();

        let published = create(&infra, &def, &file("first.txt", b"first"));
        let published_key = stored_key(&published);

        update(
            &infra,
            &def,
            &published.id,
            Some(file("second.txt", b"second")),
            true,
        );

        assert!(
            infra.storage.exists(&published_key).expect("exists"),
            "the published row still references {published_key}"
        );

        let conn = infra.pool.get().expect("connection");
        let reread = query::find_by_id(&conn, "media", &def, &published.id, None)
            .expect("find")
            .expect("the published row");

        assert_eq!(
            stored_key(&reread),
            published_key,
            "a draft save must not move the published row's file"
        );
    }

    /// Replacing the file on a published write drops the previous one when
    /// nothing else references it — with versions switched off, the row was the
    /// only reference, so the bytes go once the write commits.
    #[test]
    fn a_published_replacement_deletes_an_unreferenced_previous_file() {
        let mut unversioned = media_with_drafts();
        unversioned.versions = None;
        let (_tmp, infra, def) = infra_for(unversioned);

        let published = create(&infra, &def, &file("first.txt", b"first"));
        let first_key = stored_key(&published);

        let updated = update(
            &infra,
            &def,
            &published.id,
            Some(file("second.txt", b"second")),
            false,
        );
        let second_key = stored_key(&updated);

        assert_ne!(first_key, second_key);
        assert!(
            !infra.storage.exists(&first_key).expect("exists"),
            "the replaced file must be gone"
        );
        assert!(
            infra.storage.exists(&second_key).expect("exists"),
            "the stored file must survive the write"
        );
    }

    /// A replaced file stays in storage while a version snapshot still names
    /// it: restoring that version has to find the bytes it was saved with.
    #[test]
    fn a_replaced_file_survives_while_a_version_references_it() {
        let (_tmp, infra, def) = infra();

        let published = create(&infra, &def, &file("first.txt", b"first"));
        let first_key = stored_key(&published);

        let updated = update(
            &infra,
            &def,
            &published.id,
            Some(file("second.txt", b"second")),
            false,
        );

        assert_ne!(first_key, stored_key(&updated));
        assert!(
            infra.storage.exists(&first_key).expect("exists"),
            "the version created on upload still references {first_key}"
        );
    }

    /// Pruning the last snapshot that referenced a replaced file releases it:
    /// with a one-version cap, the replacement's own snapshot pushes the
    /// original out and nothing names the original file any more.
    #[test]
    fn pruning_the_last_version_that_referenced_a_file_deletes_it() {
        let mut capped = media_with_drafts();
        capped.versions = Some(VersionsConfig::new(true, 1));
        let (_tmp, infra, def) = infra_for(capped);

        let published = create(&infra, &def, &file("first.txt", b"first"));
        let first_key = stored_key(&published);

        let updated = update(
            &infra,
            &def,
            &published.id,
            Some(file("second.txt", b"second")),
            false,
        );

        assert!(
            !infra.storage.exists(&first_key).expect("exists"),
            "the pruned version was the last reference to {first_key}"
        );
        assert!(
            infra.storage.exists(&stored_key(&updated)).expect("exists"),
            "the published file stays"
        );
    }

    /// A drafted file that never went live is deleted once the snapshot naming
    /// it is pruned — here by a newer draft under a one-version cap. The
    /// published row's file and the published snapshot are untouched.
    #[test]
    fn a_superseded_draft_file_is_deleted_once_no_snapshot_names_it() {
        let mut capped = media_with_drafts();
        capped.versions = Some(VersionsConfig::new(true, 1));
        let (_tmp, infra, def) = infra_for(capped);

        let published = create(&infra, &def, &file("first.txt", b"first"));
        let published_key = stored_key(&published);

        let first_draft = update(
            &infra,
            &def,
            &published.id,
            Some(file("second.txt", b"second")),
            true,
        );
        let drafted_key = stored_key(&first_draft);

        let second_draft = update(
            &infra,
            &def,
            &published.id,
            Some(file("third.txt", b"third")),
            true,
        );

        assert!(
            !infra.storage.exists(&drafted_key).expect("exists"),
            "the superseded draft's file has no reference left: {drafted_key}"
        );
        assert!(
            infra.storage.exists(&published_key).expect("exists"),
            "the published row still references {published_key}"
        );
        assert!(
            infra
                .storage
                .exists(&stored_key(&second_draft))
                .expect("exists"),
            "the pending draft's file stays"
        );
    }

    /// Regression: `locale = "all"` shapes a READ — a write has no such shape,
    /// and every operation refuses it. An upload write reaches the write
    /// without going through an operation, so it accepted `all`, silently wrote
    /// the DEFAULT locale's columns under a locale the caller never named, and
    /// skipped the shared-field lock along the way.
    #[test]
    fn an_all_locales_upload_write_is_refused() {
        let (_tmp, infra, def) = infra();
        let published = create(&infra, &def, &file("first.txt", b"first"));

        let locale_ctx = LocaleContext {
            mode: LocaleMode::All,
            config: LocaleConfig {
                default_locale: "en".to_string(),
                locales: vec!["en".to_string(), "de".to_string()],
                fallback: true,
            },
        };
        let ctx = ServiceContext::collection("media", &def)
            .infra(&infra)
            .build();

        let err = update_upload(
            &ctx,
            UpdateUploadInput {
                id: &published.id,
                storage: &infra.storage,
                file: None,
                form: empty_form(&def),
                locale_ctx: Some(&locale_ctx),
                password: None,
                ui_locale: None,
                draft: false,
                upload_max_file_size: MAX_FILE_SIZE,
                image_max_attempts: FALLBACK_MAX_ATTEMPTS,
                form_echoes_locked_fields: false,
            },
        )
        .err()
        .expect("a write targets one locale");

        let ServiceError::Validation(validation) = err else {
            panic!("expected a validation error, got {err:?}");
        };
        let message = validation
            .to_field_map()
            .get("locale")
            .cloned()
            .expect("the locale field is named");
        assert!(message.contains("'all'"), "unexpected: {message}");
    }

    /// The source key of the conversion job for this document's thumbnail —
    /// the size file beside the original, under the same stem.
    fn thumbnail_source(doc: &Document) -> String {
        let key = stored_key(doc);
        let (stem, ext) = key.rsplit_once('.').expect("an extension");

        format!("{stem}_thumbnail.{ext}")
    }

    /// A draft save records its file in the snapshot and leaves everything
    /// live alone: the published row keeps its url, and the drafted file's
    /// deferred conversion is NOT queued — a job would write its derivative url
    /// onto the published row, which does not reference that file yet.
    #[test]
    fn a_draft_save_records_its_file_without_publishing_or_queueing_it() {
        let (_tmp, infra, def) = infra_for(media_with_queued_webp());

        let published = create(&infra, &def, &image_file("first.png", 40));
        let published_url = live_url(&infra, &def, &published.id);

        let drafted = update(
            &infra,
            &def,
            &published.id,
            Some(image_file("second.png", 40)),
            true,
        );

        assert_eq!(
            live_url(&infra, &def, &published.id),
            published_url,
            "a draft save must not move the published row's file"
        );
        assert_eq!(
            latest_snapshot(&infra, &published.id)["url"].as_str(),
            drafted.get_str("url"),
            "the draft snapshot carries the new file"
        );

        let payloads = queued_conversion_payloads(&infra);
        assert_eq!(payloads.len(), 1, "{payloads:?}");
        assert!(
            payloads[0].contains(&thumbnail_source(&published)),
            "only the published file's conversion is queued: {payloads:?}"
        );
    }

    /// Publishing the draft makes its file live: the published row takes the
    /// drafted url, the previous file's still-queued conversion is cancelled
    /// and the drafted file's own is queued in its place. The previous file's
    /// bytes stay — the version created on upload still references them.
    #[test]
    fn publishing_a_draft_makes_its_file_live_and_queues_its_conversions() {
        let (_tmp, infra, def) = infra_for(media_with_queued_webp());

        let published = create(&infra, &def, &image_file("first.png", 40));
        let published_key = stored_key(&published);

        let drafted = update(
            &infra,
            &def,
            &published.id,
            Some(image_file("second.png", 40)),
            true,
        );
        let drafted_url = drafted.get_str("url").expect("a url").to_string();

        update(&infra, &def, &published.id, None, false);

        assert_eq!(
            live_url(&infra, &def, &published.id),
            drafted_url,
            "the publish must carry the drafted file over to the row"
        );

        let payloads = queued_conversion_payloads(&infra);
        assert_eq!(payloads.len(), 1, "{payloads:?}");
        assert!(
            payloads[0].contains(&thumbnail_source(&drafted)),
            "the drafted file's conversion is queued: {payloads:?}"
        );
        assert!(
            !payloads[0].contains(&thumbnail_source(&published)),
            "the previous file's conversion is cancelled: {payloads:?}"
        );
        assert!(
            infra.storage.exists(&published_key).expect("exists"),
            "a version snapshot still references {published_key}"
        );
    }

    /// A publish that carries a file of its own publishes THAT file: the
    /// request's upload wins over the pending draft's.
    #[test]
    fn a_file_in_the_publishing_request_wins_over_the_drafted_one() {
        let (_tmp, infra, def) = infra_for(media_with_queued_webp());

        let published = create(&infra, &def, &image_file("first.png", 40));

        let drafted = update(
            &infra,
            &def,
            &published.id,
            Some(image_file("second.png", 40)),
            true,
        );

        let republished = update(
            &infra,
            &def,
            &published.id,
            Some(image_file("third.png", 40)),
            false,
        );

        assert_eq!(
            live_url(&infra, &def, &published.id),
            republished.get_str("url").expect("a url"),
            "the file the request carried must win"
        );

        let payloads = queued_conversion_payloads(&infra);
        assert_eq!(payloads.len(), 1, "{payloads:?}");
        assert!(
            payloads[0].contains(&thumbnail_source(&republished)),
            "only the request's own file owes a conversion: {payloads:?}"
        );
        assert!(
            !payloads[0].contains(&thumbnail_source(&drafted)),
            "the drafted file was not published: {payloads:?}"
        );
    }

    /// The rule lives in the service write, not in the multipart upload
    /// surface: a publish issued through the plain update operation (gRPC, Lua,
    /// MCP) adopts the drafted file exactly the same way.
    #[test]
    fn a_plain_update_publish_adopts_the_drafted_file() {
        let (_tmp, infra, def) = infra_for(media_with_queued_webp());

        let published = create(&infra, &def, &image_file("first.png", 40));
        let drafted = update(
            &infra,
            &def,
            &published.id,
            Some(image_file("second.png", 40)),
            true,
        );
        let drafted_url = drafted.get_str("url").expect("a url").to_string();

        let ctx = ServiceContext::collection("media", &def)
            .infra(&infra)
            .build();

        update_document(
            &ctx,
            &published.id,
            WriteInput::builder(DocumentFields::new()).build(),
        )
        .expect("publish");

        assert_eq!(live_url(&infra, &def, &published.id), drafted_url);

        let payloads = queued_conversion_payloads(&infra);
        assert_eq!(payloads.len(), 1, "{payloads:?}");
        assert!(
            payloads[0].contains(&thumbnail_source(&drafted)),
            "{payloads:?}"
        );
    }

    /// A publish that follows an edit which changed no file leaves the queued
    /// conversion alone: re-queueing would cancel the job for the very file the
    /// row keeps referencing.
    #[test]
    fn publishing_a_draft_that_changed_no_file_keeps_its_pending_conversion() {
        let (_tmp, infra, def) = infra_for(media_with_queued_webp());

        let published = create(&infra, &def, &image_file("first.png", 40));

        update(&infra, &def, &published.id, None, true);
        update(&infra, &def, &published.id, None, false);

        let payloads = queued_conversion_payloads(&infra);
        assert_eq!(payloads.len(), 1, "{payloads:?}");
        assert!(
            payloads[0].contains(&thumbnail_source(&published)),
            "the published file's conversion is untouched: {payloads:?}"
        );
    }

    /// A drafted file that never went live is still this document's file:
    /// deleting the document has to take it too. The delete removed only the
    /// published row's files, so a file that lived solely in a draft snapshot
    /// stayed in storage forever with nothing left to reference it.
    #[test]
    fn deleting_a_document_removes_a_file_only_its_draft_ever_named() {
        let (_tmp, infra, def) = infra();

        let published = create(&infra, &def, &file("first.txt", b"first"));
        let published_key = stored_key(&published);

        let drafted = update(
            &infra,
            &def,
            &published.id,
            Some(file("second.txt", b"second")),
            true,
        );
        let drafted_key = stored_key(&drafted);
        assert_ne!(published_key, drafted_key);

        let ctx = ServiceContext::collection("media", &def)
            .infra(&infra)
            .build();
        delete_document(&ctx, &published.id, Some(&*infra.storage), None).expect("delete");

        assert!(
            !infra.storage.exists(&published_key).expect("exists"),
            "the published file goes with the document"
        );
        assert!(
            !infra.storage.exists(&drafted_key).expect("exists"),
            "the drafted file goes with the document too"
        );
    }

    /// An update that carries no file changes no files.
    #[test]
    fn an_update_without_a_file_keeps_the_stored_one() {
        let (_tmp, infra, def) = infra();

        let published = create(&infra, &def, &file("first.txt", b"first"));
        let key = stored_key(&published);

        update(&infra, &def, &published.id, None, false);

        assert!(infra.storage.exists(&key).expect("exists"));
    }
}
