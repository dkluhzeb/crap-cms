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
//! The pending draft is ONE unit, and a publish makes all of it live. The
//! merge above is locale-scoped — it feeds the write the values for the locale
//! the request targets — so the draft's other translations and the shared
//! values a default-locale draft save recorded reach the row through the
//! snapshot write-back at persist time, written with the same per-locale rule a
//! restore of that snapshot uses. Publishing in `en` therefore no longer
//! strands a German draft in history, and a `de` publish still cannot *change*
//! a shared field: the locale lock judges the request's own fields, while the
//! draft's shared values simply go live as the draft author saved them.
//!
//! Publishing does not widen what the publisher may write. The adopted values
//! are the draft author's, but they enter `input.data` like any other incoming
//! field and the field-level write-access strip runs over them afterwards, so a
//! publisher who may not write a field cannot publish a drafted change to it:
//! the strip drops it and the published row keeps its own value.

use std::collections::HashSet;

use serde_json::{Map, Value};

mod file;

use file::{FilePublish, adopt_drafted_file, drafted_file_target, excluded_columns};

use crate::{
    core::{
        CollectionDefinition, FieldDefinition, GlobalDefinition, flatten_group_fields,
        nest_group_fields,
    },
    db::{DbConnection, query, query::helpers::global_table},
    service::{ServiceContext, ServiceError, WriteInput},
};

type Result<T> = std::result::Result<T, ServiceError>;

/// The one parent id a global's single row and its version history use.
const GLOBAL_PARENT: &str = "default";

/// Where a publish reads its pending draft from, and the schema that draft
/// describes.
///
/// A collection names its slug and the document id; a global names its version
/// table and the single parent every global row shares. Everything else about
/// publishing a draft is the same for both, which is why the adoption is
/// written against this rather than against a collection definition.
struct DraftSource<'a> {
    table: &'a str,
    parent_id: &'a str,
    fields: &'a [FieldDefinition],
}

/// Whether this write publishes: an update that is not a draft save, on a
/// collection or global whose drafts are recorded as version snapshots. Only
/// such a write has a pending draft to publish.
fn publishes_pending_draft(has_drafts: bool, has_versions: bool, input: &WriteInput<'_>) -> bool {
    has_drafts && has_versions && !input.draft
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
    table: &str,
    parent_id: &str,
) -> Result<Option<Map<String, Value>>> {
    Ok(query::find_latest_version(conn, table, parent_id)?
        .filter(|version| version.status == "draft")
        .and_then(|version| version.snapshot.as_object().cloned()))
}

/// Read the pending draft on a connection resolved for this call alone, so it
/// is released before any read the caller takes next — a pool-mode context
/// resolves one connection per call.
fn pending_draft_snapshot(
    ctx: &ServiceContext,
    source: &DraftSource<'_>,
) -> Result<Option<Map<String, Value>>> {
    let conn = ctx.resolve_conn()?;

    pending_draft(conn.as_ref(), source.table, source.parent_id)
}

/// Fill every field the request does not send from the pending draft.
///
/// The request's data is group-nested and the draft's base is flat columns, so
/// the merge happens in the flat form — a group sub-field the request sends
/// keeps its value while its siblings come from the draft — and the result is
/// nested again into the one shape the rest of the pipeline carries.
///
/// This half is locale-scoped like any other write: the base holds the values
/// the snapshot carries for the locale being published and, under a non-default
/// locale, nothing shared (see [`query::snapshot_write_fields`]). The draft's
/// OTHER locales reach the row through the snapshot write-back at persist time
/// — a publish makes the pending draft live as one unit.
///
/// `excluded` names columns the request's own content owns outright, so the
/// draft never contributes them.
///
/// # Errors
///
/// Returns an error if a configured locale code has no column form.
fn adopt_drafted_fields(
    source: &DraftSource<'_>,
    snapshot: &Map<String, Value>,
    excluded: &HashSet<String>,
    input: &mut WriteInput<'_>,
) -> Result<()> {
    let drafted = query::snapshot_write_fields(snapshot, source.fields, input.locale_ctx)?;

    let mut flat = flatten_group_fields(&input.data, source.fields);

    for (column, value) in drafted {
        if excluded.contains(&column) {
            continue;
        }

        flat.entry(column).or_insert(value);
    }

    input.data = nest_group_fields(&flat, source.fields);

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
/// Returns the pending draft's snapshot when there was one, so the persist step
/// can write the locales this request does not target back from it.
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
) -> Result<Option<Map<String, Value>>> {
    if !publishes_pending_draft(def.has_drafts(), def.has_versions(), input) {
        return Ok(None);
    }

    // Both are decided against the request's own data, before the draft's
    // fields are merged into it.
    let file_target = drafted_file_target(def, input);
    let excluded = excluded_columns(def, input);

    let source = DraftSource {
        table: ctx.slug,
        parent_id: id,
        fields: &def.fields,
    };

    let Some(snapshot) = pending_draft_snapshot(ctx, &source)? else {
        return Ok(None);
    };

    adopt_drafted_fields(&source, &snapshot, &excluded, input)?;

    let Some(upload) = file_target else {
        return Ok(Some(snapshot));
    };

    adopt_drafted_file(ctx, &FilePublish { def, id, upload }, &snapshot, input)?;

    Ok(Some(snapshot))
}

/// Publish a global's pending draft — the same rule, against the global's own
/// version table and its single `default` parent.
///
/// A global update used to write only the fields the request carried, so a
/// partial publish put one drafted field live and discarded the rest of the
/// draft. Globals are never upload collections (a `GlobalDefinition` has no
/// upload config at all), so there is no file half here.
///
/// # Errors
///
/// Returns a backend error if the draft snapshot cannot be read, or if a
/// configured locale code has no column form.
pub(crate) fn adopt_pending_global_draft(
    ctx: &ServiceContext,
    def: &GlobalDefinition,
    input: &mut WriteInput<'_>,
) -> Result<Option<Map<String, Value>>> {
    if !publishes_pending_draft(def.has_drafts(), def.has_versions(), input) {
        return Ok(None);
    }

    let table = global_table(ctx.slug);
    let source = DraftSource {
        table: &table,
        parent_id: GLOBAL_PARENT,
        fields: &def.fields,
    };

    let Some(snapshot) = pending_draft_snapshot(ctx, &source)? else {
        return Ok(None);
    };

    adopt_drafted_fields(&source, &snapshot, &HashSet::new(), input)?;

    Ok(Some(snapshot))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{
        config::LocaleConfig,
        core::{
            DocumentFields, FieldDefinition, FieldType, VersionsConfig,
            upload::{CollectionUpload, FormatQuality, ImageSizeBuilder},
        },
        db::{LocaleContext, LocaleMode},
        service::write::reject_locale_locked_fields,
    };

    /// A versioned, draft-enabled upload collection with a `thumbnail` size
    /// whose webp variant is converted on the queue.
    pub(super) fn media() -> CollectionDefinition {
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

    /// The draft source a collection publish builds. Only `fields` matters to
    /// the field merge; the table and parent address the version row a
    /// connected test reads.
    fn source(def: &CollectionDefinition) -> DraftSource<'_> {
        DraftSource {
            table: "posts",
            parent_id: "doc-1",
            fields: &def.fields,
        }
    }

    pub(super) fn snapshot() -> Map<String, Value> {
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

    /// Publishing is the only write with a pending draft to take as its base:
    /// a draft save records content, it does not publish it, and a collection
    /// without draft snapshots has nothing pending.
    #[test]
    fn only_a_publish_takes_the_draft_as_its_base() {
        let def = media();
        let publishes = |def: &CollectionDefinition, input: &WriteInput<'_>| {
            publishes_pending_draft(def.has_drafts(), def.has_versions(), input)
        };

        assert!(publishes(
            &def,
            &WriteInput::builder(DocumentFields::new()).build()
        ));

        assert!(
            !publishes(
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
            !publishes(
                &unversioned,
                &WriteInput::builder(DocumentFields::new()).build()
            ),
            "without versions there is no draft snapshot to adopt from"
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

        adopt_drafted_fields(&source(&def), &snapshot, &HashSet::new(), &mut input).unwrap();

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

        adopt_drafted_fields(&source(&def), &snapshot, &HashSet::new(), &mut input).unwrap();

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
            CollectionDefinition, Document, DocumentFields, FieldDefinition, FieldType, Hooks,
            Registry, ValidationError, VersionsConfig,
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

#[cfg(all(test, feature = "sqlite"))]
mod global_draft_tests {
    use rusqlite::Connection;
    use serde_json::json;

    use super::*;
    use crate::core::{DocumentFields, FieldType, VersionsConfig};

    /// A versioned, draft-enabled `settings` global with three scalar fields.
    fn settings() -> GlobalDefinition {
        let mut def = GlobalDefinition::new("settings");
        def.versions = Some(VersionsConfig::new(true, 10));
        def.fields = vec![
            FieldDefinition::builder("a", FieldType::Text).build(),
            FieldDefinition::builder("b", FieldType::Text).build(),
            FieldDefinition::builder("c", FieldType::Text).build(),
        ];

        def
    }

    /// A connection holding the global's version table and one pending draft
    /// of all three fields.
    fn drafted_settings() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE _versions__global_settings (
                id TEXT PRIMARY KEY,
                _parent TEXT NOT NULL,
                _version INTEGER NOT NULL,
                _status TEXT NOT NULL,
                _latest INTEGER NOT NULL DEFAULT 0,
                snapshot TEXT NOT NULL,
                created_at TEXT
            );",
        )
        .unwrap();

        query::create_version(
            &conn,
            "_global_settings",
            "default",
            "draft",
            &json!({ "a": "drafted a", "b": "drafted b", "c": "drafted c" }),
        )
        .unwrap();

        conn
    }

    /// A partial global publish used to write only the field it carried and
    /// discard the rest of the draft: the rule collections have always applied
    /// never reached globals at all.
    #[test]
    fn a_partial_global_publish_takes_the_whole_draft_as_its_base() {
        let conn = drafted_settings();
        let def = settings();
        let ctx = ServiceContext::global("settings", &def).conn(&conn).build();

        let request: DocumentFields = [("a".to_string(), json!("published a"))]
            .into_iter()
            .collect();
        let mut input = WriteInput::builder(request).build();

        let adopted = adopt_pending_global_draft(&ctx, &def, &mut input)
            .unwrap()
            .expect("the pending draft");

        assert_eq!(input.data.get("a"), Some(&json!("published a")));
        assert_eq!(input.data.get("b"), Some(&json!("drafted b")));
        assert_eq!(input.data.get("c"), Some(&json!("drafted c")));
        assert_eq!(
            adopted.get("a"),
            Some(&json!("drafted a")),
            "the snapshot itself comes back as stored"
        );
    }

    /// A draft save records content, it does not publish it — so it adopts
    /// nothing and leaves the request's data alone.
    #[test]
    fn a_global_draft_save_adopts_nothing() {
        let conn = drafted_settings();
        let def = settings();
        let ctx = ServiceContext::global("settings", &def).conn(&conn).build();

        let mut input = WriteInput::builder(DocumentFields::new())
            .draft(true)
            .build();

        assert!(
            adopt_pending_global_draft(&ctx, &def, &mut input)
                .unwrap()
                .is_none()
        );
        assert!(input.data.get("b").is_none());
    }

    /// Without versioning there is no draft snapshot to adopt from.
    #[test]
    fn an_unversioned_global_adopts_nothing() {
        let conn = drafted_settings();
        let mut def = settings();
        def.versions = None;
        let ctx = ServiceContext::global("settings", &def).conn(&conn).build();

        let mut input = WriteInput::builder(DocumentFields::new()).build();

        assert!(
            adopt_pending_global_draft(&ctx, &def, &mut input)
                .unwrap()
                .is_none()
        );
    }
}
