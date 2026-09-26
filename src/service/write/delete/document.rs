//! The delete lifecycle on an existing connection/transaction: access,
//! delete-hook data, reference-count protection, hooks, and file ownership.

use serde_json::Value;

use crate::{
    config::LocaleConfig,
    core::{CollectionDefinition, DocumentFields, ReqContext},
    db::{AccessResult, DbConnection, LocaleContext, query},
    hooks::{AccessCheckInput, HookEvent},
    service::{
        DeleteEvent, ServiceContext, ServiceError,
        helpers::enforce_access_constraints,
        hooks::WriteHooks,
        write::{delete::execute::execute_delete, owned_file_keys},
    },
};

type Result<T> = std::result::Result<T, ServiceError>;

/// Result of a delete operation.
pub(crate) struct DeleteResult {
    /// Request-scoped context returned by after-delete hooks.
    pub context: ReqContext,
    /// The storage keys the deleted document referenced — the published row's
    /// AND every version snapshot's — for post-commit cleanup. A file only a
    /// draft ever named is still this document's file and goes with it.
    ///
    /// Empty for a collection without uploads and for a soft delete, which
    /// keeps every file so an undelete finds them.
    pub upload_keys: Vec<String>,
    /// The delete's live event — the view the removed row was last in and its
    /// gating snapshot (see [`DeleteEvent`]). `None` when the delete publishes
    /// no event.
    pub event: Option<DeleteEvent>,
}

/// Build the delete-hook `data`: the document's full fields (when a delete
/// hook will run) plus `id`, and `soft_delete` for a soft delete; otherwise
/// just `{ id }` (+ `soft_delete`).
///
/// The document is loaded only when a delete hook runs, so a plain delete on a
/// hook-less collection does no extra query here. Upload cleanup does not read
/// the row here — its keys come from [`document_file_keys`], which covers the
/// snapshots too.
fn prepare_delete_hook_data(
    ctx: &ServiceContext,
    write_hooks: &dyn WriteHooks,
    def: &CollectionDefinition,
    conn: &dyn DbConnection,
    id: &str,
    locale_config: Option<&LocaleConfig>,
) -> Result<DocumentFields> {
    let mut hook_data = if write_hooks.runs_delete_hooks(&def.hooks) {
        let lc = locale_config.cloned().unwrap_or_default();
        let locale_ctx = LocaleContext::default_for(&lc);

        // Propagate a genuine DB error rather than swallowing it — a
        // transient failure here must not silently degrade delete hooks to
        // `{ id }` only (losing upload-cleanup fields) and look like "not
        // found". `?` on the query; `Option` still means genuinely absent.
        query::find_by_id(conn, ctx.slug, def, id, locale_ctx.as_ref())?
            .map(|d| d.fields)
            .unwrap_or_default()
    } else {
        DocumentFields::new()
    };

    hook_data.insert("id".to_string(), Value::String(id.to_string()));

    if def.soft_delete {
        hook_data.insert("soft_delete".to_string(), Value::Bool(true));
    }

    Ok(hook_data)
}

/// Judge the delete against its access rule: `trash` for a soft delete,
/// `delete` for a hard one — including the row-level constraints a rule may
/// return. Runs on the locked row (see [`delete_document_in_conn`]).
fn admit_delete(
    ctx: &ServiceContext,
    write_hooks: &dyn WriteHooks,
    def: &CollectionDefinition,
    id: &str,
) -> Result<()> {
    let (operation, access_ref, op_label) = if def.soft_delete {
        ("trash", def.access.resolve_trash(), "Trash")
    } else {
        ("delete", def.access.delete.as_ref(), "Delete")
    };

    // Delete is locale-agnostic — the whole row is removed across all locales.
    // The operation label matches the access fn invoked (and the admin
    // permission grid).
    let access = write_hooks.check_access(
        &AccessCheckInput::builder(operation, ctx.slug)
            .access(access_ref)
            .user(ctx.user)
            .id(Some(id))
            .build(),
    )?;

    if matches!(access, AccessResult::Denied) {
        return Err(ServiceError::AccessDenied(format!(
            "{op_label} access denied"
        )));
    }

    // Constrained filters must match the row. The target row is live (a soft
    // delete moves it to the trash, a hard delete removes it — both start from
    // the live view).
    enforce_access_constraints(ctx, id, &access, op_label, false)
}

/// Refuse a hard delete of a document other documents still reference.
fn refuse_if_referenced(conn: &dyn DbConnection, slug: &str, id: &str) -> Result<()> {
    let count = query::ref_count::get_ref_count_locked(conn, slug, id)?.unwrap_or(0);

    if count == 0 {
        return Ok(());
    }

    Err(ServiceError::Referenced {
        id: id.to_string(),
        count,
    })
}

/// The files this document owns — the published row's AND every version
/// snapshot's, so a file only a never-published draft ever named goes with
/// the document instead of staying behind forever. A soft delete resolves
/// none: an undelete brings the document back and must find its files.
fn files_to_release(
    conn: &dyn DbConnection,
    def: &CollectionDefinition,
    id: &str,
    locale_cfg: &LocaleConfig,
) -> Result<Vec<String>> {
    if def.soft_delete {
        return Ok(Vec::new());
    }

    let purge_locale = LocaleContext::default_for(locale_cfg);

    owned_file_keys(conn, def, id, purge_locale.as_ref())
}

/// Lock the row, judge the delete on it, and take the delete-hook snapshot.
///
/// The row is locked before it is judged, as every state-changing write does:
/// the access constraints, the delete-hook snapshot and the reference count
/// must all see the row the delete lands on — not one a concurrent writer
/// reassigns or publishes in between. The lock is a no-op on `SQLite`, whose
/// `IMMEDIATE` transaction serializes writers already.
///
/// The snapshot is read once, before the delete removes the row; for a hard
/// delete it is `after_delete`'s only view of what was removed.
fn admit_locked(
    ctx: &ServiceContext,
    conn: &dyn DbConnection,
    id: &str,
    locale_config: Option<&LocaleConfig>,
) -> Result<DocumentFields> {
    let write_hooks = ctx.write_hooks()?;
    let def = ctx.collection_def()?;

    conn.lock_row(ctx.slug, id)?;

    admit_delete(ctx, write_hooks, def, id)?;

    let hook_data = prepare_delete_hook_data(ctx, write_hooks, def, conn, id, locale_config)?;

    if !def.soft_delete {
        refuse_if_referenced(conn, ctx.slug, id)?;
    }

    Ok(hook_data)
}

/// The delete hooks of one document, run on the delete's connection.
struct DeleteHooks<'a, 'c> {
    ctx: &'a ServiceContext<'c>,
    conn: &'a dyn DbConnection,
    id: &'a str,
}

impl DeleteHooks<'_, '_> {
    /// Run `event`'s hooks on `data`, carrying `context` from an earlier
    /// hook; returns the context they leave.
    fn run(
        &self,
        event: HookEvent,
        data: DocumentFields,
        context: Option<ReqContext>,
    ) -> Result<ReqContext> {
        let write_hooks = self.ctx.write_hooks()?;
        let def = self.ctx.collection_def()?;

        let mut builder = self
            .ctx
            .hook_context("delete")
            .data(data)
            .document_id(self.id);

        if let Some(context) = context {
            builder = builder.context(context);
        }

        let result =
            write_hooks.run_hooks_with_conn(&def.hooks, event, builder.build(), self.conn)?;

        Ok(result.context)
    }
}

/// Delete a document on an existing connection/transaction.
///
/// Runs the full lifecycle: row lock -> access -> ref count check ->
/// before-delete hooks -> delete -> cleanup -> after-delete hooks.
/// Does NOT manage transactions — caller must open/commit.
/// Upload file cleanup is returned as `upload_keys` for the caller to handle after commit.
///
/// Hard-deleting an auth document revokes that user's sessions; the stream
/// invalidation is published post-commit by the wrappers
/// (`delete_document_pool` / `_conn`, `delete_many_*`), so a rollback cannot
/// leave a phantom invalidation.
pub(crate) fn delete_document_in_conn(
    ctx: &ServiceContext,
    id: &str,
    locale_config: Option<&LocaleConfig>,
) -> Result<DeleteResult> {
    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();
    let def = ctx.collection_def()?;

    let hook_data = admit_locked(ctx, conn, id, locale_config)?;

    let hooks = DeleteHooks { ctx, conn, id };
    let context = hooks.run(HookEvent::BeforeDelete, hook_data.clone(), None)?;

    let locale_cfg = locale_config.cloned().unwrap_or_default();

    // Read after the before-hooks, so one that rewrote the row through its own
    // CRUD is accounted for, and before the delete removes both the row and
    // its versions.
    let upload_keys = files_to_release(conn, def, id, &locale_cfg)?;

    let event = execute_delete(ctx, conn, id, &locale_cfg)?;

    let context = hooks.run(HookEvent::AfterDelete, hook_data, Some(context))?;

    Ok(DeleteResult {
        context,
        upload_keys,
        event,
    })
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use std::sync::{Arc, Mutex};

    use rusqlite::Connection;
    use serde_json::json;

    use super::*;
    use crate::{
        core::{
            FieldDefinition, FieldType, Hooks, SharedInvalidationTransport, ValidationError,
            event::InProcessInvalidationBus,
        },
        db::query::test_helpers::CountingConn,
        hooks::{HookContext, ValidationCtx},
        service::{
            FieldReadStrip, delete_document,
            write::delete::test_support::{AllowAllWriteHooks, setup_auth_collection},
        },
    };

    #[tokio::test]
    async fn hard_delete_auth_publishes_user_invalidation() {
        let (conn, def) = setup_auth_collection();
        let bus = Arc::new(InProcessInvalidationBus::new());
        let transport: SharedInvalidationTransport = bus;
        let mut rx = transport.subscribe();

        let hooks = AllowAllWriteHooks;
        let ctx = ServiceContext::collection("users", &def)
            .conn(&conn)
            .write_hooks(&hooks)
            .override_access(true)
            .invalidation_transport(Some(transport))
            .build();

        // Invalidation is published by the wrapper (conn mode → delete_document_conn),
        // post-commit, not inside delete_document_in_conn.
        let _ = delete_document(&ctx, "u1", None, None).expect("delete");

        let received = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .expect("recv timed out")
            .expect("expected invalidation signal");
        assert_eq!(received, "u1");
    }

    /// Regression: soft-deleting an auth user MUST tear down their live streams.
    /// A trashed user is disabled — the per-request evaluator resolves users via
    /// `find_by_id`, which excludes soft-deleted rows, so the user's existing
    /// sessions are already rejected (`UserMissing`); their open SSE/subscribe
    /// streams (which never re-resolve) must be torn down too. Previously the
    /// delete wrapper skipped teardown on soft-delete on the mistaken assumption
    /// that "the row still exists, so the session still resolves."
    #[tokio::test]
    async fn soft_delete_auth_publishes_user_invalidation() {
        let (conn, mut def) = setup_auth_collection();
        // soft_delete requires the _deleted_at column.
        conn.execute_batch("ALTER TABLE users ADD COLUMN _deleted_at TEXT;")
            .unwrap();
        def.soft_delete = true;

        let bus = Arc::new(InProcessInvalidationBus::new());
        let transport: SharedInvalidationTransport = bus;
        let mut rx = transport.subscribe();

        let hooks = AllowAllWriteHooks;
        let ctx = ServiceContext::collection("users", &def)
            .conn(&conn)
            .write_hooks(&hooks)
            .override_access(true)
            .invalidation_transport(Some(transport))
            .build();

        let _ = delete_document(&ctx, "u1", None, None).expect("soft delete");

        let received = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .expect("recv timed out")
            .expect("soft-delete of an auth user must publish an invalidation signal");
        assert_eq!(received, "u1");
    }

    #[tokio::test]
    async fn hard_delete_non_auth_does_not_publish() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                _revision INTEGER NOT NULL DEFAULT 0,
                title TEXT,
                _ref_count INTEGER DEFAULT 0,
                created_at TEXT,
                updated_at TEXT
            );
            INSERT INTO posts (id, title) VALUES ('p1', 'hi');",
        )
        .unwrap();

        let mut def = CollectionDefinition::new("posts");
        def.timestamps = true;
        def.fields = vec![FieldDefinition::builder("title", FieldType::Text).build()];

        let bus = Arc::new(InProcessInvalidationBus::new());
        let transport: SharedInvalidationTransport = bus;
        let mut rx = transport.subscribe();

        let hooks = AllowAllWriteHooks;
        let ctx = ServiceContext::collection("posts", &def)
            .conn(&conn)
            .write_hooks(&hooks)
            .override_access(true)
            .invalidation_transport(Some(transport))
            .build();

        let _ = delete_document_in_conn(&ctx, "p1", None).expect("delete");

        let recv_result =
            tokio::time::timeout(std::time::Duration::from_millis(150), rx.recv()).await;
        assert!(
            recv_result.is_err(),
            "non-auth hard-delete must not publish an invalidation signal"
        );
    }

    /// Allow-all hooks that record the `data` passed to `before_delete` /
    /// `after_delete`, so a test can assert the document's field data reaches
    /// the delete-hook context. `runs_delete_hooks` returns `true` so the
    /// document is pre-loaded.
    #[derive(Default)]
    struct RecordingWriteHooks {
        before: Mutex<Vec<DocumentFields>>,
        after: Mutex<Vec<DocumentFields>>,
        ui_locales: Mutex<Vec<Option<String>>>,
    }

    impl WriteHooks for RecordingWriteHooks {
        fn runs_delete_hooks(&self, _hooks: &Hooks) -> bool {
            true
        }

        fn run_before_write(
            &self,
            _hooks: &Hooks,
            _fields: &[FieldDefinition],
            ctx: HookContext,
            _val_ctx: &ValidationCtx,
        ) -> anyhow::Result<HookContext> {
            Ok(ctx)
        }

        fn run_after_write(
            &self,
            _hooks: &Hooks,
            _fields: &[FieldDefinition],
            _event: HookEvent,
            ctx: HookContext,
            _conn: &dyn DbConnection,
        ) -> anyhow::Result<HookContext> {
            Ok(ctx)
        }

        fn run_hooks_with_conn(
            &self,
            _hooks: &Hooks,
            event: HookEvent,
            ctx: HookContext,
            _conn: &dyn DbConnection,
        ) -> anyhow::Result<HookContext> {
            match event {
                HookEvent::BeforeDelete => self.before.lock().unwrap().push(ctx.data.clone()),
                HookEvent::AfterDelete => self.after.lock().unwrap().push(ctx.data.clone()),
                _ => {}
            }

            self.ui_locales.lock().unwrap().push(ctx.ui_locale.clone());

            Ok(ctx)
        }

        fn check_access(&self, _input: &AccessCheckInput<'_>) -> anyhow::Result<AccessResult> {
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

    impl FieldReadStrip for RecordingWriteHooks {}

    /// Regression: `before_delete` and `after_delete` must receive the deleted
    /// document's field data (plus `id`), not just `{ id }`. For a hard delete
    /// the row is gone by `after_delete`, so the snapshot is the only way the
    /// hook can see what was removed.
    #[test]
    fn delete_hooks_receive_document_field_data() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                _revision INTEGER NOT NULL DEFAULT 0,
                title TEXT,
                _ref_count INTEGER DEFAULT 0,
                created_at TEXT,
                updated_at TEXT
            );
            INSERT INTO posts (id, title) VALUES ('p1', 'Hello');",
        )
        .unwrap();

        let mut def = CollectionDefinition::new("posts");
        def.timestamps = true;
        def.fields = vec![FieldDefinition::builder("title", FieldType::Text).build()];

        let hooks = RecordingWriteHooks::default();
        let ctx = ServiceContext::collection("posts", &def)
            .conn(&conn)
            .write_hooks(&hooks)
            .override_access(true)
            .build();

        delete_document_in_conn(&ctx, "p1", None).expect("delete");

        let before = hooks.before.lock().unwrap();
        let after = hooks.after.lock().unwrap();
        assert_eq!(before.len(), 1, "before_delete should fire once");
        assert_eq!(after.len(), 1, "after_delete should fire once");

        assert_eq!(
            before[0].get("title"),
            Some(&json!("Hello")),
            "before_delete must see the document's field data"
        );
        assert_eq!(
            before[0].get("id"),
            Some(&json!("p1")),
            "before_delete must still carry the id"
        );
        assert_eq!(
            after[0].get("title"),
            Some(&json!("Hello")),
            "after_delete must see the field data (the row is already gone)"
        );
    }

    /// Regression: the delete hooks were built without the admin UI locale, so
    /// a hook localizing its message through `ctx.ui_locale` answered in the
    /// default language on delete only.
    #[test]
    fn delete_hooks_receive_the_ui_locale() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                _revision INTEGER NOT NULL DEFAULT 0,
                title TEXT,
                _ref_count INTEGER DEFAULT 0,
                created_at TEXT,
                updated_at TEXT
            );
            INSERT INTO posts (id, title) VALUES ('p1', 'Hello');",
        )
        .unwrap();

        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![FieldDefinition::builder("title", FieldType::Text).build()];

        let hooks = RecordingWriteHooks::default();
        let ctx = ServiceContext::collection("posts", &def)
            .conn(&conn)
            .write_hooks(&hooks)
            .override_access(true)
            .ui_locale(Some("de".to_string()))
            .build();

        delete_document_in_conn(&ctx, "p1", None).expect("delete");

        let seen = hooks.ui_locales.lock().unwrap();
        assert_eq!(seen.len(), 2, "before_delete and after_delete both fire");
        assert!(
            seen.iter().all(|l| l.as_deref() == Some("de")),
            "every delete hook sees ctx.ui_locale, got {seen:?}"
        );
    }

    /// A `posts` table with one live row `p1`, and `trash` adding the trash
    /// column.
    fn posts_with_row(trash: bool) -> (Connection, CollectionDefinition) {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                _revision INTEGER NOT NULL DEFAULT 0,
                title TEXT,
                _ref_count INTEGER DEFAULT 0,
                _deleted_at TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            INSERT INTO posts (id, title) VALUES ('p1', 'Hello');",
        )
        .unwrap();

        let mut def = CollectionDefinition::new("posts");
        def.soft_delete = trash;
        def.fields = vec![FieldDefinition::builder("title", FieldType::Text).build()];

        (conn, def)
    }

    /// Regression: a delete and a trash judged the access rule's row
    /// constraints — and read the delete-hook snapshot — before taking any
    /// lock. On Postgres a writer that reassigned or published the row in
    /// between let the delete remove a document the rule forbids. The row is
    /// now locked before anything is read, as an update does.
    #[test]
    fn delete_and_trash_lock_the_row_before_reading_it() {
        for trash in [false, true] {
            let (raw, def) = posts_with_row(trash);
            let conn = CountingConn::new(&raw);
            let hooks = RecordingWriteHooks::default();
            let ctx = ServiceContext::collection("posts", &def)
                .conn(&conn)
                .write_hooks(&hooks)
                .override_access(true)
                .build();

            delete_document_in_conn(&ctx, "p1", None).expect("delete");

            assert_eq!(
                conn.locks(),
                vec![("posts".to_string(), "p1".to_string())],
                "trash={trash}: the row is locked once"
            );
            assert_eq!(
                conn.reads_at_locks(),
                vec![0],
                "trash={trash}: nothing is read before the lock"
            );
        }
    }
}
