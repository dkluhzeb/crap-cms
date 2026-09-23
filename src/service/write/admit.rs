//! The gate every document update passes before its before-write hooks run:
//! the document row is locked, the admission prefix (canonicalize, adopt the
//! pending draft as the write's base, locale lock) runs, the `update` access
//! rule is applied, and write-denied fields are stripped from the request and
//! from the draft it publishes. The single-document update and the bulk update
//! share it, so a rule enforced on one cannot be missing on the other.

use serde_json::Value;

use crate::{
    db::{DbConnection, LocaleContext},
    service::{
        ServiceContext, ServiceError, WriteInput,
        write::{admit_update_input, check_update_access, stored_fields_for_update_rules},
    },
};

/// Admit an update: lock the document row, run the admission prefix
/// (canonicalize, adopt the pending draft, locale lock), apply the `update`
/// access rule, then strip write-denied fields.
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

    // The same prefix the `validate` dry-run runs, so the two judge one input.
    let pending_draft = admit_update_input(ctx, def, id, input)?;

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

    pending_draft.publishing_snapshot(ctx, write_hooks, &stored, input.locale_ctx)
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use anyhow::Result as AnyResult;
    use rusqlite::Connection;
    use serde_json::json;

    use super::*;
    use crate::{
        core::{
            CollectionDefinition, DocumentFields, FieldDefinition, FieldType, Hooks,
            ValidationError, VersionsConfig,
        },
        db::{AccessResult, query, query::test_helpers::CountingConn},
        hooks::{AccessCheckInput, HookContext, HookEvent, ValidationCtx},
        service::{FieldReadStrip, WriteHooks},
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
