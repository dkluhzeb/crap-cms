//! Publishing a pending draft: the draft is the base, the request is the edit.
//!
//! An update with `draft = false` on a collection that has drafts and versions
//! publishes whatever the last draft save recorded. It rarely restates that
//! content — the admin form's Publish button, a `crap.update(…, { draft =
//! false })`, a gRPC/MCP publish all send only what the caller changed — so the
//! write takes the latest draft snapshot as its base and applies the request's
//! fields on top. The request wins on every field it sends; every field it does
//! not send comes from the draft. Without this a publish wrote the request's
//! handful of fields onto the OLD row and silently discarded the rest of the
//! draft.
//!
//! The draft's file is one case of the same rule. A draft save on an upload
//! collection stores the file and records its server-derived columns (`url`,
//! `filename`, the per-size urls and dimensions) in the snapshot; the published
//! row, its file and that file's queued conversions are left alone. Those
//! columns are read back from the snapshot server-side — the same values the
//! upload pipeline computed, never a hidden form field — and the conversions
//! the draft deferred are re-derived from the stored size columns at the same
//! moment, so a variant job only ever exists for a file the published row
//! actually references.
//!
//! Publishing does not widen what the publisher may write. The adopted values
//! are the draft author's, but they enter `input.data` like any other incoming
//! field and the field-level write-access strip runs over them afterwards, so a
//! publisher who may not write a field cannot publish a drafted change to it:
//! the strip drops it and the published row keeps its own value.

use serde_json::{Map, Value};

use crate::{
    core::{
        CollectionDefinition, DocumentFields, flatten_group_fields, nest_group_fields,
        upload::{CollectionUpload, QueuedConversion, key_from_served_url},
    },
    db::{DbConnection, query},
    service::{ServiceContext, ServiceError, UploadConversions, WriteInput, write::stored_row},
};

type Result<T> = std::result::Result<T, ServiceError>;

/// Whether this write publishes: an update that is not a draft save, on a
/// collection whose drafts are recorded as version snapshots. Only such a write
/// has a pending draft to publish.
fn publishes_pending_draft(def: &CollectionDefinition, input: &WriteInput<'_>) -> bool {
    def.has_drafts() && def.has_versions() && !input.draft
}

/// The document whose drafted file a publish is about to make live. Every
/// field is required and it is built at the one call site, so a plain literal
/// stands in for a builder.
#[derive(Clone, Copy)]
struct FilePublish<'a> {
    def: &'a CollectionDefinition,
    id: &'a str,
    upload: &'a CollectionUpload,
}

/// The upload configuration whose drafted file this publish makes live, if any.
///
/// A write that carries `url` processed a file of its own (the multipart
/// handlers inject the derived columns before the write): that file is the one
/// the caller asked for, so it wins and none is adopted. Decided against the
/// request's own data, before the draft's fields are merged in.
///
/// A non-default-locale write is excluded for the same reason it cannot write
/// any other shared column: the file columns are not localized, so a
/// translation neither records a file in its draft nor publishes one.
fn drafted_file_target<'a>(
    def: &'a CollectionDefinition,
    input: &WriteInput<'_>,
) -> Option<&'a CollectionUpload> {
    if !def.is_upload_collection()
        || input.data.contains_key("url")
        || query::is_non_default_single_locale(input.locale_ctx)
    {
        return None;
    }

    def.upload.as_ref()
}

/// The latest version snapshot, when it is a pending draft.
///
/// The same snapshot a draft save builds on, so what publishes is what the last
/// draft save recorded — every earlier draft was folded into it. A publish
/// records a `published` snapshot as the new latest version, which is what
/// makes this `None` for the next write: the draft is no longer pending, and
/// the draft save after a publish starts from the published row.
fn pending_draft(
    conn: &dyn DbConnection,
    slug: &str,
    id: &str,
) -> Result<Option<Map<String, Value>>> {
    Ok(query::find_latest_version(conn, slug, id)?
        .filter(|version| version.status == "draft")
        .and_then(|version| version.snapshot.as_object().cloned()))
}

/// Fill every field the request does not send from the pending draft.
///
/// The request's data is group-nested and the draft's base is flat columns, so
/// the merge happens in the flat form — a group sub-field the request sends
/// keeps its value while its siblings come from the draft — and the result is
/// nested again into the one shape the rest of the pipeline carries.
///
/// Locale-scoped like any other write: the base holds the values the snapshot
/// carries for the locale being published and, under a non-default locale,
/// nothing shared (see [`query::snapshot_write_fields`]).
///
/// # Errors
///
/// Returns an error if a configured locale code has no column form.
fn adopt_drafted_fields(
    def: &CollectionDefinition,
    snapshot: &Map<String, Value>,
    input: &mut WriteInput<'_>,
) -> Result<()> {
    let drafted = query::snapshot_write_fields(snapshot, &def.fields, input.locale_ctx)?;

    let mut flat = flatten_group_fields(&input.data, &def.fields);

    for (column, value) in drafted {
        flat.entry(column).or_insert(value);
    }

    input.data = nest_group_fields(&flat, &def.fields);

    Ok(())
}

/// The server-derived upload columns `snapshot` carries.
///
/// Sorted, so the write applies them in a stable order regardless of how the
/// derived-name set iterates.
fn drafted_metadata(snapshot: &Map<String, Value>, upload: &CollectionUpload) -> DocumentFields {
    let mut names: Vec<String> = upload.derived_field_names().into_iter().collect();
    names.sort();

    names
        .into_iter()
        .filter_map(|name| {
            let value = snapshot.get(&name)?.clone();

            Some((name, value))
        })
        .collect()
}

/// The conversions the drafted file still owes.
///
/// Re-derived from the size columns the metadata carries rather than stored
/// with the draft: a deferred variant is a function of the stored size file and
/// the collection's format options, and deriving it here means the publish
/// queues exactly the jobs the current configuration calls for.
fn deferred_conversions(
    metadata: &DocumentFields,
    upload: &CollectionUpload,
) -> Vec<QueuedConversion> {
    let mut queued = Vec::new();

    for size in &upload.image_sizes {
        let Some(size_key) = metadata
            .get_str(&format!("{}_url", size.name))
            .and_then(key_from_served_url)
        else {
            continue;
        };

        for (format, opts) in upload.format_options.deferred() {
            queued.push(QueuedConversion::for_size(
                size_key,
                &size.name,
                format,
                opts.quality,
            ));
        }
    }

    queued
}

/// Carry the pending draft's file over to the published row.
///
/// Adds the drafted server-derived columns to `input.data` and attaches the
/// conversions that file still owes, which makes the write settle them: the
/// previous file's still-queued variants are cancelled and the new ones queued,
/// exactly as a write that carried the file itself would.
///
/// Nothing happens when the draft did not change the file — re-queueing there
/// would cancel the conversions still pending for the very file the row keeps.
///
/// # Errors
///
/// Returns a backend error if the published row cannot be read. It must
/// propagate: publishing the old file while reporting success is silent data
/// loss.
fn adopt_drafted_file(
    ctx: &ServiceContext,
    target: &FilePublish<'_>,
    snapshot: &Map<String, Value>,
    input: &mut WriteInput<'_>,
) -> Result<()> {
    let FilePublish { def, id, upload } = *target;

    let drafted = drafted_metadata(snapshot, upload);
    let Some(drafted_url) = drafted.get_str("url").map(str::to_string) else {
        return Ok(());
    };

    let live_url = stored_row(ctx, def, id, input.locale_ctx)?
        .and_then(|doc| doc.fields.get_str("url").map(str::to_string));

    if live_url.as_deref() == Some(drafted_url.as_str()) {
        return Ok(());
    }

    input.upload_conversions = Some(UploadConversions::new(
        deferred_conversions(&drafted, upload),
        ctx.image_max_attempts,
    ));
    input.data.extend(drafted);

    Ok(())
}

/// Publish the pending draft: take it as the write's base and let the request's
/// own fields win over it.
///
/// Runs at the write chokepoint, after the incoming data is canonicalized and
/// after untrusted upload metadata has been stripped from it, and before the
/// locale lock, the access gates and validation — so everything the draft
/// contributes is judged exactly like a field the caller sent.
///
/// # Errors
///
/// Returns a backend error if the draft snapshot or the published row cannot be
/// read, or if a configured locale code has no column form. It must propagate:
/// publishing the old content while reporting success is silent data loss.
pub(crate) fn adopt_pending_draft(
    ctx: &ServiceContext,
    def: &CollectionDefinition,
    id: &str,
    input: &mut WriteInput<'_>,
) -> Result<()> {
    if !publishes_pending_draft(def, input) {
        return Ok(());
    }

    let file_target = drafted_file_target(def, input);

    // Scoped so the connection is released before the row read below takes
    // its own — a pool-mode context resolves one per call.
    let pending = {
        let conn = ctx.resolve_conn()?;

        pending_draft(conn.as_ref(), ctx.slug, id)?
    };

    let Some(snapshot) = pending else {
        return Ok(());
    };

    adopt_drafted_fields(def, &snapshot, input)?;

    let Some(upload) = file_target else {
        return Ok(());
    };

    adopt_drafted_file(ctx, &FilePublish { def, id, upload }, &snapshot, input)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{
        config::LocaleConfig,
        core::{
            FieldDefinition, FieldType, VersionsConfig,
            upload::{FormatQuality, ImageSizeBuilder},
        },
        db::{LocaleContext, LocaleMode},
        service::write::reject_locale_locked_fields,
    };

    /// A versioned, draft-enabled upload collection with a `thumbnail` size
    /// whose webp variant is converted on the queue.
    fn media() -> CollectionDefinition {
        let mut upload = CollectionUpload::new();
        upload.image_sizes = vec![
            ImageSizeBuilder::new("thumbnail")
                .width(300)
                .height(300)
                .build(),
        ];
        upload.format_options.webp = Some(FormatQuality::new(80, true));

        let mut def = CollectionDefinition::new("media");
        def.upload = Some(upload);
        def.versions = Some(VersionsConfig::new(true, 10));

        def
    }

    fn snapshot() -> Map<String, Value> {
        json!({
            "url": "/uploads/media/abc_photo.png",
            "filename": "abc_photo.png",
            "thumbnail_url": "/uploads/media/abc_photo_thumbnail.png",
            "thumbnail_width": 300,
            "caption": "not a file column"
        })
        .as_object()
        .expect("object")
        .clone()
    }

    /// Only the server-derived columns are taken from the snapshot; a user
    /// field it also carries is left to the write's own data.
    #[test]
    fn drafted_metadata_takes_only_the_derived_columns() {
        let def = media();
        let upload = def.upload.as_ref().expect("upload");

        let drafted = drafted_metadata(&snapshot(), upload);

        assert_eq!(
            drafted.get("url"),
            Some(&json!("/uploads/media/abc_photo.png"))
        );
        assert_eq!(drafted.get("filename"), Some(&json!("abc_photo.png")));
        assert_eq!(drafted.get("thumbnail_width"), Some(&json!(300)));
        assert!(
            !drafted.contains_key("caption"),
            "a user field is not server-derived: {drafted:?}"
        );
    }

    /// The deferred variant is derived from the stored size file, so the job
    /// converts the drafted thumbnail — not the published row's.
    #[test]
    fn deferred_conversions_target_the_drafted_size_file() {
        let def = media();
        let upload = def.upload.as_ref().expect("upload");
        let drafted = drafted_metadata(&snapshot(), upload);

        let queued = deferred_conversions(&drafted, upload);

        assert_eq!(queued.len(), 1, "{queued:?}");
        assert_eq!(queued[0].source_path, "media/abc_photo_thumbnail.png");
        assert_eq!(queued[0].target_path, "media/abc_photo_thumbnail.webp");
        assert_eq!(queued[0].url_column, "thumbnail_webp_url");
    }

    /// A format converted during the upload owes the queue nothing — only a
    /// `queue = true` variant is deferred to the publish.
    #[test]
    fn a_synchronously_converted_format_queues_nothing() {
        let mut def = media();
        def.upload.as_mut().expect("upload").format_options.webp =
            Some(FormatQuality::new(80, false));

        let upload = def.upload.as_ref().expect("upload");

        let drafted = drafted_metadata(&snapshot(), upload);

        assert!(deferred_conversions(&drafted, upload).is_empty());
    }

    /// Publishing is the only write with a pending draft to take as its base:
    /// a draft save records content, it does not publish it, and a collection
    /// without draft snapshots has nothing pending.
    #[test]
    fn only_a_publish_takes_the_draft_as_its_base() {
        let def = media();

        assert!(publishes_pending_draft(
            &def,
            &WriteInput::builder(DocumentFields::new()).build()
        ));

        assert!(
            !publishes_pending_draft(
                &def,
                &WriteInput::builder(DocumentFields::new())
                    .draft(true)
                    .build()
            ),
            "a draft save records content, it does not publish it"
        );

        let mut unversioned = media();
        unversioned.versions = None;
        assert!(
            !publishes_pending_draft(
                &unversioned,
                &WriteInput::builder(DocumentFields::new()).build()
            ),
            "without versions there is no draft snapshot to adopt from"
        );
    }

    /// The file adoption is narrower than the field adoption: a request that
    /// processed a file of its own wins, a translation never publishes a file,
    /// and a collection without uploads has no file columns at all.
    #[test]
    fn only_a_publish_without_its_own_file_adopts_the_drafted_file() {
        let def = media();

        assert!(
            drafted_file_target(&def, &WriteInput::builder(DocumentFields::new()).build())
                .is_some()
        );

        let own_file: DocumentFields = [("url".to_string(), json!("/uploads/media/new.png"))]
            .into_iter()
            .collect();
        assert!(
            drafted_file_target(&def, &WriteInput::builder(own_file).build()).is_none(),
            "the file the request carried wins"
        );

        let de = LocaleContext {
            mode: LocaleMode::Single("de".to_string()),
            config: LocaleConfig {
                default_locale: "en".to_string(),
                locales: vec!["en".to_string(), "de".to_string()],
                fallback: true,
            },
        };
        assert!(
            drafted_file_target(
                &def,
                &WriteInput::builder(DocumentFields::new())
                    .locale_ctx(Some(&de))
                    .build()
            )
            .is_none(),
            "the file columns are shared — a translation cannot publish one"
        );

        assert!(
            drafted_file_target(
                &CollectionDefinition::new("posts"),
                &WriteInput::builder(DocumentFields::new()).build()
            )
            .is_none(),
            "a collection without uploads has no file columns"
        );
    }

    /// The request wins on every field it sends — including a group sub-field,
    /// where its siblings still come from the draft — and every field it leaves
    /// out is filled from the draft, join rows included.
    #[test]
    fn the_request_wins_per_field_and_the_draft_fills_the_rest() {
        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("body", FieldType::Text).build(),
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("heading", FieldType::Text).build(),
                    FieldDefinition::builder("robots", FieldType::Text).build(),
                ])
                .build(),
            FieldDefinition::builder("slides", FieldType::Array).build(),
        ];

        let snapshot = json!({
            "title": "drafted title",
            "body": "drafted body",
            "seo": { "heading": "drafted heading", "robots": "noindex" },
            "slides": [{ "id": "r1", "caption": "drafted slide" }],
            "_status": "draft",
        })
        .as_object()
        .expect("object")
        .clone();

        let request: DocumentFields = [
            ("title".to_string(), json!("published title")),
            ("seo".to_string(), json!({ "heading": "published heading" })),
        ]
        .into_iter()
        .collect();
        let mut input = WriteInput::builder(request).build();

        adopt_drafted_fields(&def, &snapshot, &mut input).unwrap();

        assert_eq!(input.data.get("title"), Some(&json!("published title")));
        assert_eq!(input.data.get("body"), Some(&json!("drafted body")));
        assert_eq!(
            input.data.get("seo"),
            Some(&json!({ "heading": "published heading", "robots": "noindex" })),
        );
        assert_eq!(
            input.data.get("slides"),
            Some(&json!([{ "id": "r1", "caption": "drafted slide" }])),
        );
        assert!(
            !input.data.contains_key("_status"),
            "the publish decides the status, never the snapshot: {:?}",
            input.data
        );
    }

    /// A non-default-locale publish adopts that locale's drafted values and
    /// leaves every shared field alone — it may not write one, and the locale
    /// lock rejects the write outright if one is present.
    #[test]
    fn a_non_default_locale_publish_adopts_only_that_locales_values() {
        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text)
                .localized(true)
                .build(),
            FieldDefinition::builder("slug", FieldType::Text).build(),
        ];

        let snapshot = json!({
            "title": "English",
            "title__en": "English",
            "title__de": "Deutsch",
            "slug": "shared-slug",
        })
        .as_object()
        .expect("object")
        .clone();

        let de = LocaleContext {
            mode: LocaleMode::Single("de".to_string()),
            config: LocaleConfig {
                default_locale: "en".to_string(),
                locales: vec!["en".to_string(), "de".to_string()],
                fallback: true,
            },
        };
        let mut input = WriteInput::builder(DocumentFields::new())
            .locale_ctx(Some(&de))
            .build();

        adopt_drafted_fields(&def, &snapshot, &mut input).unwrap();

        assert_eq!(input.data.get("title"), Some(&json!("Deutsch")));
        assert!(!input.data.contains_key("slug"), "{:?}", input.data);
        reject_locale_locked_fields(&def.fields, &input.data, Some(&de)).unwrap();
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod publish_tests {
    use anyhow::Result as AnyResult;
    use serde_json::{Value, json};

    use super::*;
    use crate::{
        config::CrapConfig,
        core::{
            CollectionDefinition, Document, FieldDefinition, FieldType, Hooks, Registry,
            ValidationError, VersionsConfig,
        },
        db::{AccessResult, DbConnection, DbPool, migrate, pool, query},
        hooks::{AccessCheckInput, HookContext, HookEvent, ValidationCtx},
        service::{
            FieldReadStrip, ServiceContext, create_document_in_conn, hooks::WriteHooks,
            update_document_in_conn,
        },
    };

    /// Write hooks that run nothing and allow every access check.
    ///
    /// `denied_field` names a field this writer may not write. The field-level
    /// write strip drops it from the patch — the one mechanism through which a
    /// publisher's own access limits reach the values adopted from the draft.
    struct TestWriteHooks {
        denied_field: Option<&'static str>,
    }

    impl TestWriteHooks {
        fn new(denied_field: Option<&'static str>) -> Self {
            Self { denied_field }
        }
    }

    impl WriteHooks for TestWriteHooks {
        fn run_before_write(
            &self,
            _hooks: &Hooks,
            _fields: &[FieldDefinition],
            ctx: HookContext,
            _val_ctx: &ValidationCtx,
        ) -> AnyResult<HookContext> {
            Ok(ctx)
        }

        fn run_after_write(
            &self,
            _hooks: &Hooks,
            _fields: &[FieldDefinition],
            _event: HookEvent,
            ctx: HookContext,
            _conn: &dyn DbConnection,
        ) -> AnyResult<HookContext> {
            Ok(ctx)
        }

        fn run_hooks_with_conn(
            &self,
            _hooks: &Hooks,
            _event: HookEvent,
            ctx: HookContext,
            _conn: &dyn DbConnection,
        ) -> AnyResult<HookContext> {
            Ok(ctx)
        }

        fn check_access(&self, _input: &AccessCheckInput<'_>) -> AnyResult<AccessResult> {
            Ok(AccessResult::Allowed)
        }

        fn validate_fields(
            &self,
            _fields: &[FieldDefinition],
            _data: &DocumentFields,
            _ctx: &ValidationCtx,
        ) -> std::result::Result<(), ValidationError> {
            Ok(())
        }

        fn strip_write_access_update(
            &self,
            _fields: &[FieldDefinition],
            data: &mut DocumentFields,
            _stored: &DocumentFields,
            _collection: &str,
            _user: Option<&Document>,
            _locale: Option<&str>,
        ) {
            if let Some(field) = self.denied_field {
                data.remove(field);
            }
        }
    }

    impl FieldReadStrip for TestWriteHooks {}

    fn data(pairs: &[(&str, Value)]) -> DocumentFields {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect()
    }

    /// A versioned, draft-enabled `posts` collection with a scalar field, a
    /// write-protected one and an array.
    fn posts() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("posts");
        def.versions = Some(VersionsConfig::new(true, 10));
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("body", FieldType::Text).build(),
            FieldDefinition::builder("secret", FieldType::Text).build(),
            FieldDefinition::builder("slides", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("caption", FieldType::Text).build(),
                ])
                .build(),
        ];

        def
    }

    fn migrated_posts() -> (tempfile::TempDir, DbPool, CollectionDefinition) {
        let def = posts();

        let tmp = tempfile::tempdir().unwrap();
        let mut config = CrapConfig::test_default();
        config.database.path = "test.db".to_string();
        let db_pool = pool::create_pool(tmp.path(), &config).unwrap();

        let shared = Registry::shared();
        shared.write().unwrap().register_collection(def.clone());
        migrate::sync_all(&db_pool, &shared.read().unwrap(), &config.locale).unwrap();

        (tmp, db_pool, def)
    }

    /// The captions of the document's stored array rows, in order.
    fn stored_captions(
        conn: &dyn DbConnection,
        def: &CollectionDefinition,
        id: &str,
    ) -> Vec<String> {
        let sub = &def.fields[3].fields;

        query::find_array_rows(conn, "posts", "slides", id, sub, None)
            .unwrap()
            .into_iter()
            .map(|row| row["caption"].as_str().unwrap_or_default().to_string())
            .collect()
    }

    /// Publishing applies the request on top of the pending draft: the field
    /// the request sends wins, and every field it leaves out — a scalar and the
    /// array's rows alike — is published as the draft recorded it. A publish
    /// used to write only the fields the request carried, silently discarding
    /// the rest of the draft.
    #[test]
    fn a_publish_carrying_one_field_publishes_the_whole_draft() {
        let (_tmp, db_pool, def) = migrated_posts();
        let conn = db_pool.get().unwrap();
        let hooks = TestWriteHooks::new(None);
        let ctx = ServiceContext::collection("posts", &def)
            .conn(&conn)
            .write_hooks(&hooks)
            .build();

        let (created, _) = create_document_in_conn(
            &ctx,
            WriteInput::builder(data(&[
                ("title", json!("live title")),
                ("body", json!("live body")),
                ("slides", json!([{ "caption": "live slide" }])),
            ]))
            .build(),
        )
        .unwrap();
        let id = created.id.to_string();

        update_document_in_conn(
            &ctx,
            &id,
            WriteInput::builder(data(&[
                ("title", json!("drafted title")),
                ("body", json!("drafted body")),
                ("slides", json!([{ "caption": "drafted slide" }])),
            ]))
            .draft(true)
            .build(),
        )
        .unwrap();

        let (published, _) = update_document_in_conn(
            &ctx,
            &id,
            WriteInput::builder(data(&[("title", json!("published title"))])).build(),
        )
        .unwrap();

        assert_eq!(published.get_str("title"), Some("published title"));
        assert_eq!(
            published.get_str("body"),
            Some("drafted body"),
            "a field the request left out publishes from the draft"
        );
        assert_eq!(
            stored_captions(&conn, &def, &id),
            vec!["drafted slide".to_string()],
            "the draft's array rows publish too"
        );
    }

    /// Once a publish has landed the draft is no longer pending: the latest
    /// version is the published one, so a second publish has nothing to adopt
    /// and the next draft save starts from the published row rather than the
    /// stale draft.
    #[test]
    fn the_draft_stops_being_pending_once_it_is_published() {
        let (_tmp, db_pool, def) = migrated_posts();
        let conn = db_pool.get().unwrap();
        let hooks = TestWriteHooks::new(None);
        let ctx = ServiceContext::collection("posts", &def)
            .conn(&conn)
            .write_hooks(&hooks)
            .build();

        let (created, _) = create_document_in_conn(
            &ctx,
            WriteInput::builder(data(&[
                ("title", json!("live title")),
                ("body", json!("live body")),
            ]))
            .build(),
        )
        .unwrap();
        let id = created.id.to_string();

        update_document_in_conn(
            &ctx,
            &id,
            WriteInput::builder(data(&[("body", json!("drafted body"))]))
                .draft(true)
                .build(),
        )
        .unwrap();

        update_document_in_conn(
            &ctx,
            &id,
            WriteInput::builder(data(&[("title", json!("published title"))])).build(),
        )
        .unwrap();

        let latest = query::find_latest_version(&conn, "posts", &id)
            .unwrap()
            .expect("a version");
        assert_eq!(
            latest.status, "published",
            "the publish records the new latest version"
        );

        update_document_in_conn(
            &ctx,
            &id,
            WriteInput::builder(data(&[("body", json!("second drafted body"))]))
                .draft(true)
                .build(),
        )
        .unwrap();

        let snapshot = query::find_latest_version(&conn, "posts", &id)
            .unwrap()
            .expect("a version")
            .snapshot;
        assert_eq!(
            snapshot["title"],
            json!("published title"),
            "the next draft builds on the published row: {snapshot}"
        );
        assert_eq!(snapshot["body"], json!("second drafted body"), "{snapshot}");
    }

    /// Publishing does not widen what the publisher may write. A field the
    /// publisher is write-denied on is stripped from the merged data like any
    /// other incoming field, so the drafted change to it does not go live and
    /// the row keeps its own value.
    #[test]
    fn a_publisher_denied_on_a_field_cannot_publish_its_drafted_change() {
        let (_tmp, db_pool, def) = migrated_posts();
        let conn = db_pool.get().unwrap();
        let author = TestWriteHooks::new(None);
        let author_ctx = ServiceContext::collection("posts", &def)
            .conn(&conn)
            .write_hooks(&author)
            .build();

        let (created, _) = create_document_in_conn(
            &author_ctx,
            WriteInput::builder(data(&[
                ("title", json!("live title")),
                ("secret", json!("live secret")),
            ]))
            .build(),
        )
        .unwrap();
        let id = created.id.to_string();

        update_document_in_conn(
            &author_ctx,
            &id,
            WriteInput::builder(data(&[
                ("body", json!("drafted body")),
                ("secret", json!("drafted secret")),
            ]))
            .draft(true)
            .build(),
        )
        .unwrap();

        let publisher = TestWriteHooks::new(Some("secret"));
        let publisher_ctx = ServiceContext::collection("posts", &def)
            .conn(&conn)
            .write_hooks(&publisher)
            .build();

        let (written, _) = update_document_in_conn(
            &publisher_ctx,
            &id,
            WriteInput::builder(data(&[("title", json!("written title"))])).build(),
        )
        .unwrap();

        assert_eq!(written.get_str("title"), Some("written title"));
        assert_eq!(
            written.get_str("body"),
            Some("drafted body"),
            "the fields the publisher MAY write still publish"
        );
        assert_eq!(
            written.get_str("secret"),
            Some("live secret"),
            "the write-denied field keeps the row's value"
        );
    }
}
