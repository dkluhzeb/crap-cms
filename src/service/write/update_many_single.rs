//! Core per-document update for bulk operations (partial update, no password).
//! Honors `draft` the same way the single-document update does: a draft save
//! is routed to the version table and leaves the published main row untouched,
//! and a publish takes the document's pending draft as its base.

use serde_json::Value;

use crate::{
    config::LocaleConfig,
    db::LocaleContext,
    hooks::{HookContext, ValidationCtx},
    service::{
        AfterChangeInput, Gated, PersistOptions, ServiceContext, WriteInput, WriteResult,
        persist_bulk_update, persist_draft_version, run_after_change_hooks,
    },
};

use super::ServiceError;
use super::admit::admit_update;
use crate::service::helpers::{hydrate_reported, strip_reported};
use crate::service::write::{UploadSettle, document_file_keys, settle_upload_write};

type Result<T> = std::result::Result<T, ServiceError>;

/// Update a single document in a bulk operation (partial update).
///
/// Runs the full lifecycle: access check -> field stripping -> before-write hooks ->
/// partial persist -> hydrate -> after-write hooks -> read-denied stripping.
/// Returns the stored row the document's live event is built from alongside
/// the result. Does NOT manage transactions — caller must
/// open/commit.
pub(crate) fn update_many_single_in_conn(
    ctx: &ServiceContext,
    id: &str,
    mut input: WriteInput<'_>,
    locale_config: &LocaleConfig,
) -> Result<Gated<WriteResult>> {
    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();
    let write_hooks = ctx.write_hooks()?;
    let def = ctx.collection_def()?;

    // The same admission as the single-document update: canonicalize, adopt
    // the pending draft, locale lock, `update` access, write strip — a bulk
    // publish must not be a forgery hole nor discard the drafts a single
    // publish carries over.
    let publishing_draft = admit_update(ctx, conn, id, &mut input)?;

    let is_draft = input.draft && def.has_drafts();

    let hook_data = input.data.clone();
    let hook_ctx = HookContext::builder(ctx.slug, "update")
        .data(hook_data)
        .document_id(id)
        .locale(input.locale_ctx.map(LocaleContext::access_locale))
        .draft(is_draft)
        .user(ctx.user)
        .ui_locale(input.ui_locale.as_deref())
        .build();

    // Same rule as the single-document update: a publish writes the draft's
    // other locales back after validation, so completeness judges that
    // snapshot and not the locales it replaces.
    let val_ctx = ValidationCtx::builder(conn, ctx.slug)
        .exclude_id(Some(id))
        .draft(is_draft)
        .locale_ctx(input.locale_ctx)
        .soft_delete(def.soft_delete)
        .collection_required_locales(def.required_locales.as_ref())
        .user(ctx.user)
        .ui_locale(input.ui_locale.as_deref())
        .locale_overlay(publishing_draft.as_ref().and_then(Value::as_object))
        .build();

    let final_ctx = write_hooks.run_before_write(&def.hooks, &def.fields, hook_ctx, &val_ctx)?;

    // A draft bulk update routes to the version table (main row untouched),
    // exactly like the single-document update path — otherwise `draft = true`
    // would silently publish the change by writing the main row.
    let snapshot_only = is_draft && def.has_versions();

    // The files the document references going in — the published row's and
    // every version snapshot's — so the ones nothing references any more can be
    // dropped once the write has landed. Read last, so a before-hook that
    // rewrote the row through its own CRUD is accounted for.
    let before_files = document_file_keys(ctx, def, id, input.locale_ctx)?;

    // A draft save reports its snapshot with its own rows; a published write
    // reports the stored row, hydrated BEFORE after-change hooks so they see
    // nested data.
    let mut doc = if snapshot_only {
        let doc = persist_draft_version(ctx, id, &final_ctx.data, input.locale_ctx)?;

        // What can still go is a file whose last reference was a snapshot this
        // save's pruning removed; the published row and its files are untouched.
        settle_upload_write(
            ctx,
            &UploadSettle::builder(def, id)
                .before(Some(&before_files))
                .build(),
        )?;

        doc
    } else {
        let final_data = final_ctx.to_value_map();
        let opts = PersistOptions::builder()
            .locale_ctx(input.locale_ctx)
            .locale_config(Some(locale_config))
            .pending_draft(publishing_draft.as_ref().and_then(Value::as_object))
            .build();

        let mut doc = persist_bulk_update(ctx, id, &final_data, &opts)?;

        // The same settle the single-document update runs. A bulk publish
        // adopted the drafted file but settled nothing, so its conversions were
        // never queued, the previous file's were never cancelled, and the
        // previous file was never released — and no later write could still see
        // it, so those bytes stayed in storage forever.
        settle_upload_write(
            ctx,
            &UploadSettle::builder(def, id)
                .before(Some(&before_files))
                .updated_row(Some(&doc.fields))
                .conversions(input.upload_conversions.as_ref())
                .build(),
        )?;

        hydrate_reported(ctx, &mut doc, input.locale_ctx)?;
        doc
    };

    let after_ctx = run_after_change_hooks(
        write_hooks,
        &def.hooks,
        &def.fields,
        &doc,
        AfterChangeInput::builder(ctx.slug, "update")
            .locale(
                input
                    .locale_ctx
                    .map(LocaleContext::access_locale)
                    .map(String::from),
            )
            .draft(is_draft)
            .req_context(final_ctx.context)
            .user(ctx.user)
            .ui_locale(input.ui_locale.as_deref())
            .build(),
        conn,
    )?;

    // The row as stored, before anything is shaped or stripped for the writer:
    // the live event is built from it.
    let row = ctx.event_row(&doc);

    strip_reported(ctx, write_hooks, &mut doc, input.locale_ctx)?;

    Ok(((doc, after_ctx), row))
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use anyhow::Result;
    use rusqlite::Connection;
    use serde_json::json;

    use super::*;
    use crate::{
        core::{
            CollectionDefinition, DocumentFields, FieldDefinition, FieldType, Hooks,
            ValidationError, collection::VersionsConfig, upload::CollectionUpload,
        },
        db::{AccessResult, DbConnection, query, query::test_helpers::CountingConn},
        hooks::{AccessCheckInput, HookContext, HookEvent, ValidationCtx},
        service::{FieldReadStrip, hooks::WriteHooks},
    };

    struct NoopWriteHooks;

    impl WriteHooks for NoopWriteHooks {
        fn run_before_write(
            &self,
            _hooks: &Hooks,
            _fields: &[FieldDefinition],
            ctx: HookContext,
            _val_ctx: &ValidationCtx,
        ) -> Result<HookContext> {
            Ok(ctx)
        }

        fn run_after_write(
            &self,
            _hooks: &Hooks,
            _fields: &[FieldDefinition],
            _event: HookEvent,
            ctx: HookContext,
            _conn: &dyn DbConnection,
        ) -> Result<HookContext> {
            Ok(ctx)
        }

        fn run_hooks_with_conn(
            &self,
            _hooks: &Hooks,
            _event: HookEvent,
            ctx: HookContext,
            _conn: &dyn DbConnection,
        ) -> Result<HookContext> {
            Ok(ctx)
        }

        fn check_access(&self, _input: &AccessCheckInput<'_>) -> Result<AccessResult> {
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
    }

    impl FieldReadStrip for NoopWriteHooks {}

    fn versioned_collection() -> (Connection, CollectionDefinition) {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                title TEXT,
                _status TEXT DEFAULT 'published',
                _ref_count INTEGER DEFAULT 0,
                created_at TEXT,
                updated_at TEXT
            );
            CREATE TABLE _versions_posts (
                id TEXT PRIMARY KEY,
                _parent TEXT,
                _version INTEGER,
                _status TEXT,
                _latest INTEGER DEFAULT 0,
                snapshot TEXT,
                created_at TEXT
            );
            INSERT INTO posts (id, title, _status) VALUES ('p1', 'Original', 'published');",
        )
        .unwrap();

        let mut def = CollectionDefinition::new("posts");
        def.timestamps = true;
        def.fields = vec![FieldDefinition::builder("title", FieldType::Text).build()];
        def.versions = Some(VersionsConfig::new(true, 0));

        (conn, def)
    }

    /// Regression: a bulk update with `draft = true` on a versioned collection
    /// must route to the version table and leave the published main row
    /// untouched — exactly like the single-document update. Before the fix the
    /// bulk path wrote the main row, silently publishing the change.
    #[test]
    fn bulk_draft_update_does_not_touch_published_main_row() {
        let (conn, def) = versioned_collection();
        let wh = NoopWriteHooks;
        let ctx = ServiceContext::collection("posts", &def)
            .conn(&conn)
            .write_hooks(&wh)
            .build();

        let mut data = DocumentFields::new();
        data.insert("title".into(), json!("Edited"));
        let input = WriteInput::builder(data).draft(true).build();

        update_many_single_in_conn(&ctx, "p1", input, &LocaleConfig::default()).unwrap();

        // Main row is unchanged and still published.
        let row = DbConnection::query_one(
            &conn,
            "SELECT title, _status FROM posts WHERE id = 'p1'",
            &[],
        )
        .unwrap()
        .unwrap();
        assert_eq!(row.get_string("title").unwrap(), "Original");
        assert_eq!(row.get_string("_status").unwrap(), "published");

        // A draft version captured the edit.
        let versions = query::list_versions(&conn, "posts", "p1", false, None, None).unwrap();
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].status, "draft");
        assert_eq!(
            versions[0].snapshot.get("title").and_then(|v| v.as_str()),
            Some("Edited")
        );
    }

    /// The bulk path is admitted through the same gate as the single-document
    /// update, so it locks the document row before it reads anything the write
    /// builds on: the pending draft it publishes and the files it may drop.
    #[test]
    fn bulk_update_locks_the_row_before_it_reads_anything() {
        let (conn, mut def) = versioned_collection();
        def.upload = Some(CollectionUpload::new());

        let spy = CountingConn::new(&conn);
        let wh = NoopWriteHooks;
        let ctx = ServiceContext::collection("posts", &def)
            .conn(&spy)
            .write_hooks(&wh)
            .build();

        update_many_single_in_conn(
            &ctx,
            "p1",
            WriteInput::builder(DocumentFields::new()).build(),
            &LocaleConfig::default(),
        )
        .unwrap();

        assert_eq!(
            spy.locks().first(),
            Some(&("posts".to_string(), "p1".to_string()))
        );
        assert_eq!(
            spy.reads_at_locks().first(),
            Some(&0),
            "the row is locked before the first read, not after it"
        );
    }

    /// Publishing means the same thing in bulk as on a single document: the
    /// pending draft is the base and the request's fields win over it. The bulk
    /// path wrote only what the request carried, so a bulk publish discarded
    /// exactly the drafted content a single publish carries over.
    #[test]
    fn bulk_publish_takes_the_pending_draft_as_its_base() {
        let (conn, def) = versioned_collection();
        let wh = NoopWriteHooks;
        let ctx = ServiceContext::collection("posts", &def)
            .conn(&conn)
            .write_hooks(&wh)
            .build();

        let mut drafted = DocumentFields::new();
        drafted.insert("title".into(), json!("Drafted"));
        update_many_single_in_conn(
            &ctx,
            "p1",
            WriteInput::builder(drafted).draft(true).build(),
            &LocaleConfig::default(),
        )
        .unwrap();

        // The publish carries no title of its own.
        update_many_single_in_conn(
            &ctx,
            "p1",
            WriteInput::builder(DocumentFields::new()).build(),
            &LocaleConfig::default(),
        )
        .unwrap();

        let row = DbConnection::query_one(
            &conn,
            "SELECT title, _status FROM posts WHERE id = 'p1'",
            &[],
        )
        .unwrap()
        .unwrap();
        assert_eq!(row.get_string("title").unwrap(), "Drafted");
        assert_eq!(row.get_string("_status").unwrap(), "published");
    }
}
