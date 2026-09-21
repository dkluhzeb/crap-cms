//! The gate every document update passes before its before-write hooks run:
//! the document row is locked, the pending draft is adopted as the write's
//! base, the locale lock and the `update` access rule are applied, and
//! write-denied fields are stripped from the request and from the draft it
//! publishes. The single-document update and the bulk update share it, so a
//! rule enforced on one cannot be missing on the other.

use serde_json::{Map, Value};

use crate::{
    core::{CollectionDefinition, DocumentFields},
    db::{DbConnection, LocaleContext},
    service::{
        ServiceContext, ServiceError, WriteInput,
        hooks::{SnapshotLocales, WriteHooks},
        write::{
            adopt_pending_draft, check_update_access, reject_locale_locked_fields,
            stored_fields_for_update_rules,
        },
    },
};

use super::validate::canonicalize_write_input;

/// Admit an update: lock the document row, canonicalize, adopt the pending
/// draft, apply the locale lock and the `update` access rule, then strip
/// write-denied fields.
///
/// Returns the drafted snapshot the publish writes back — already stripped by
/// the publisher's field-level write access — or `None` when no draft is
/// pending or this is not a publish.
///
/// # Errors
///
/// Returns a backend error if the row cannot be locked, the locale-lock
/// validation error, or the access denial.
pub(super) fn admit_update(
    ctx: &ServiceContext,
    conn: &dyn DbConnection,
    id: &str,
    input: &mut WriteInput<'_>,
) -> Result<Option<Value>, ServiceError> {
    // Held from here to commit: every read this write builds on (the pending
    // draft, the stored row the access rules judge, the files it may drop)
    // must see the same state the write lands on, and the before-write hooks
    // run inside it. A hook that writes a second document therefore takes
    // that row's lock while holding this one; Postgres breaks such a cycle by
    // aborting one side with a retryable error.
    let def = ctx.collection_def()?;
    let write_hooks = ctx.write_hooks()?;

    // Serialize concurrent writers of this document before the write reads
    // anything it builds on. The pending draft below is read with a plain
    // SELECT, so without the lock a publisher can adopt a snapshot a
    // concurrent draft save has already superseded and write that stale
    // content back as the published version, burying the newer draft. Every
    // later read of this document — the stored row the access rules judge, the
    // files the write may drop, the outgoing-ref snapshot at persist time —
    // sits behind the same lock. No-op on SQLite, whose IMMEDIATE transaction
    // serializes writers already.
    conn.lock_row(ctx.slug, id)?;

    // Canonicalize incoming data to nested groups up front (idempotent); the
    // whole pipeline sees one shape, the DB edge flattens to columns.
    canonicalize_write_input(input, def);

    // Publishing takes the pending draft as the write's base and lets the
    // request's own fields win over it — the file that draft stored included,
    // whose server-derived columns come from the snapshot, read here after the
    // strip so they are the server's own values and not something a caller
    // sent. Everything the draft contributes then passes the locale lock, the
    // access gates and validation exactly like a field the caller sent.
    let pending_draft = adopt_pending_draft(ctx, def, id, input)?;

    reject_locale_locked_fields(&def.fields, &input.data, input.locale_ctx)?;

    check_update_access(
        ctx,
        write_hooks,
        def,
        id,
        &input.data,
        input.locale_ctx.map(LocaleContext::access_locale),
        input.ui_locale.as_deref(),
    )?;

    // Strip write-denied fields before hook processing (data-aware: each
    // `access.update` rule sees `ctx.data` = its level and `ctx.document` = the
    // stored document, never the patch it is judging).
    let stored = stored_fields_for_update_rules(conn, ctx.slug, def, id, input.locale_ctx)?;
    write_hooks.strip_write_access_update(
        &def.fields,
        &mut input.data,
        &stored,
        ctx.slug,
        ctx.user,
        input.locale_ctx.map(LocaleContext::access_locale),
    );

    Ok(strip_publishing_draft(
        ctx,
        write_hooks,
        def,
        &stored,
        pending_draft,
        SnapshotLocales::for_write(input.locale_ctx),
    ))
}

/// The pending draft goes live as ONE unit, so the locales the request does
/// not target take their values from the snapshot too. The publisher's own
/// field-level write access decides there as well: the same rules that just
/// stripped the merged data run over the snapshot, judged — like every
/// `access.update` rule — against the stored row rather than the content they
/// are judging. Without this strip the write-back would publish exactly the
/// drafted change the request strip refused.
fn strip_publishing_draft(
    ctx: &ServiceContext,
    write_hooks: &dyn WriteHooks,
    def: &CollectionDefinition,
    stored: &DocumentFields,
    pending_draft: Option<Map<String, Value>>,
    locales: SnapshotLocales<'_>,
) -> Option<Value> {
    let mut snapshot = Value::Object(pending_draft?);

    write_hooks.strip_write_access_value(
        &def.fields,
        &mut snapshot,
        stored,
        ctx.slug,
        ctx.user,
        locales,
    );

    Some(snapshot)
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use anyhow::Result as AnyResult;
    use rusqlite::Connection;
    use serde_json::json;

    use super::*;
    use crate::{
        core::{FieldDefinition, FieldType, Hooks, ValidationError, VersionsConfig},
        db::{AccessResult, query, query::test_helpers::CountingConn},
        hooks::{AccessCheckInput, HookContext, HookEvent, ValidationCtx},
        service::FieldReadStrip,
    };

    /// Write hooks that run nothing and allow every access check.
    struct NoopWriteHooks;

    impl WriteHooks for NoopWriteHooks {
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
    }

    impl FieldReadStrip for NoopWriteHooks {}

    /// A versioned, draft-enabled `posts` collection with one scalar field.
    fn posts() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("posts");
        def.versions = Some(VersionsConfig::new(true, 10));
        def.fields = vec![FieldDefinition::builder("title", FieldType::Text).build()];

        def
    }

    /// A published `posts` row with a newer draft pending on it.
    fn drafted_posts() -> Connection {
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
                _parent TEXT,
                _version INTEGER,
                _status TEXT,
                _latest INTEGER DEFAULT 0,
                snapshot TEXT,
                created_at TEXT
            );
            INSERT INTO posts (id, title) VALUES ('p1', 'Published');",
        )
        .unwrap();

        let drafted = json!({ "title": "Drafted" });
        query::create_version(&conn, "posts", "p1", "draft", &drafted).unwrap();

        conn
    }

    /// The document row is locked before the publish reads the pending draft
    /// it is about to make live. The draft is read with a plain SELECT, so an
    /// unlocked publisher could build its published version from a snapshot a
    /// concurrent draft save had already superseded and bury that newer draft.
    #[test]
    fn a_publish_locks_the_row_before_it_reads_the_pending_draft() {
        let conn = drafted_posts();
        let spy = CountingConn::new(&conn);
        let def = posts();
        let hooks = NoopWriteHooks;
        let ctx = ServiceContext::collection("posts", &def)
            .conn(&spy)
            .write_hooks(&hooks)
            .build();

        let mut input = WriteInput::builder(DocumentFields::new()).build();

        let adopted = admit_update(&ctx, &spy, "p1", &mut input).unwrap();

        assert!(adopted.is_some(), "the publish adopted the pending draft");
        assert_eq!(input.data.get("title"), Some(&json!("Drafted")));
        assert_eq!(spy.locks(), vec![("posts".to_string(), "p1".to_string())]);
        assert_eq!(
            spy.reads_at_locks(),
            vec![0],
            "the row lock is taken before the version-table read, not after it"
        );
        assert!(
            spy.reads() > 0,
            "the draft snapshot was read under the lock"
        );
    }

    /// The lock is not conditional on there being a draft to adopt: a draft
    /// save contends with the publish that reads its snapshot, so it takes the
    /// same lock.
    #[test]
    fn a_draft_save_locks_the_row_too() {
        let conn = drafted_posts();
        let spy = CountingConn::new(&conn);
        let def = posts();
        let hooks = NoopWriteHooks;
        let ctx = ServiceContext::collection("posts", &def)
            .conn(&spy)
            .write_hooks(&hooks)
            .build();

        let mut input = WriteInput::builder(DocumentFields::new())
            .draft(true)
            .build();

        let adopted = admit_update(&ctx, &spy, "p1", &mut input).unwrap();

        assert!(adopted.is_none(), "a draft save publishes nothing");
        assert_eq!(spy.locks(), vec![("posts".to_string(), "p1".to_string())]);
        assert_eq!(spy.reads_at_locks(), vec![0]);
    }
}
