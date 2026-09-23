//! Core delete operation for collections.

use serde_json::Value;
use tracing::warn;

use crate::{
    config::LocaleConfig,
    core::{
        CollectionDefinition, DocumentFields, ReqContext, upload::delete_image_jobs_for_document,
    },
    db::{AccessResult, DbConnection, LocaleContext, query},
    hooks::{AccessCheckInput, HookContext, HookEvent},
    service::{
        DeleteEvent, ServiceContext, helpers::enforce_access_constraints, hooks::WriteHooks,
        read_delete_event, write::owned_file_keys,
    },
};

use super::ServiceError;

type Result<T> = std::result::Result<T, ServiceError>;

/// Permanently delete a document's row with every cleanup a hard delete needs:
/// its outgoing reference counts, the row, its full-text entry and its queued
/// image conversions. The one path the service hard delete, the CLI trash purge
/// and the scheduled retention purge share. Returns whether a row was deleted.
///
/// # Errors
///
/// Returns a backend error if the reference-count update, the DELETE or the
/// full-text removal fails.
pub(crate) fn purge_document(
    conn: &dyn DbConnection,
    def: &CollectionDefinition,
    id: &str,
    locale_config: &LocaleConfig,
) -> Result<bool> {
    let slug = &def.slug;

    query::ref_count::before_hard_delete(conn, slug, id, &def.fields, locale_config)?;

    if !query::delete(conn, slug, id)? {
        return Ok(false);
    }

    if conn.supports_fts() {
        query::fts::fts_delete(conn, slug, id)?;
    }

    cancel_image_jobs(conn, slug, def, id);

    Ok(true)
}

/// Cancel an upload document's queued image conversions, so none runs against
/// a row that is gone — or against a file it has since replaced. Best-effort,
/// logged. Shared by the hard delete and the upload file-replace write.
pub(crate) fn cancel_image_jobs(
    conn: &dyn DbConnection,
    slug: &str,
    def: &CollectionDefinition,
    id: &str,
) {
    if def.is_upload_collection() {
        let _ = delete_image_jobs_for_document(conn, slug, id)
            .inspect_err(|e| warn!("Failed to cancel image jobs for {slug}/{id}: {e}"));
    }
}

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

/// The delete's live event, read on the delete's own connection (inside its
/// transaction, after the before-hooks and the hard delete's ref-count row
/// lock) so it is exactly the row the delete acts on. `None` when the delete
/// publishes no event — nothing is read then.
fn read_event(
    ctx: &ServiceContext,
    conn: &dyn DbConnection,
    id: &str,
    locale_ctx: Option<&LocaleContext>,
) -> Result<Option<DeleteEvent>> {
    if !ctx.publishes_events() {
        return Ok(None);
    }

    read_delete_event(conn, ctx.collection_def()?, id, locale_ctx)
}

/// Remove the row — trash it (soft delete) or purge it (hard delete) — and
/// return the delete's live event: a hard-deleted row as read just before it
/// went, a trashed row as it now sits in the trash.
fn execute_delete(
    ctx: &ServiceContext,
    conn: &dyn DbConnection,
    id: &str,
    locale_cfg: &LocaleConfig,
) -> Result<Option<DeleteEvent>> {
    let def = ctx.collection_def()?;
    let locale_ctx = LocaleContext::default_for(locale_cfg);

    if !def.soft_delete {
        let removed = read_event(ctx, conn, id, locale_ctx.as_ref())?;

        if !purge_document(conn, def, id, locale_cfg)? {
            return Err(ServiceError::NotFound(format!(
                "Document '{id}' not found in '{}'",
                ctx.slug
            )));
        }

        return Ok(removed);
    }

    if !query::soft_delete(conn, ctx.slug, id)? {
        return Err(ServiceError::NotFound(format!(
            "Document '{id}' not found in '{}' (or already deleted)",
            ctx.slug
        )));
    }

    // A soft-deleted row keeps its FTS entry so the trash view stays
    // searchable (the normal view is filtered by `_deleted_at` before the FTS
    // membership clause), and its queued image conversions: a restore brings
    // the upload back, and nothing re-queues them. A conversion that runs
    // while the row is trashed writes its URL onto the trashed row and
    // publishes nothing (the report reads live rows only); only a hard
    // delete cancels them.
    read_event(ctx, conn, id, locale_ctx.as_ref())
}

/// Delete a document on an existing connection/transaction.
///
/// Runs the full lifecycle: ref count check -> before-delete hooks -> delete -> cleanup -> after-delete hooks.
/// Does NOT manage transactions — caller must open/commit.
/// Upload file cleanup is returned as `upload_keys` for the caller to handle after commit.
pub(crate) fn delete_document_in_conn(
    ctx: &ServiceContext,
    id: &str,
    locale_config: Option<&LocaleConfig>,
) -> Result<DeleteResult> {
    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();
    let write_hooks = ctx.write_hooks()?;
    let def = ctx.collection_def()?;

    // Collection-level access check — use trash access for soft delete, delete for hard
    let access_ref = if def.soft_delete {
        def.access.resolve_trash()
    } else {
        def.access.delete.as_ref()
    };

    // Delete is locale-agnostic — the whole row is removed across all locales.
    // A soft delete is a "trash" operation (gated by the trash access fn);
    // a hard delete is "delete". Keeps the operation label consistent with
    // the access fn being invoked (and with the admin permission grid).
    let access = write_hooks.check_access(
        &AccessCheckInput::builder(if def.soft_delete { "trash" } else { "delete" }, ctx.slug)
            .access(access_ref)
            .user(ctx.user)
            .id(Some(id))
            .build(),
    )?;

    if matches!(access, AccessResult::Denied) {
        let msg = if def.soft_delete {
            "Trash access denied"
        } else {
            "Delete access denied"
        };

        return Err(ServiceError::AccessDenied(msg.into()));
    }

    // When the hook returned Constrained filters, enforce the row-level match
    // before deleting. The target row is live (soft-delete moves it to trash,
    // hard delete removes it — both start from the live view).
    let op_label = if def.soft_delete { "Trash" } else { "Delete" };
    enforce_access_constraints(ctx, id, &access, op_label, false)?;

    // Load the document fields once (before deletion removes the row) for the
    // delete-hook context, and build the hook `data`. For a hard delete the row
    // is gone afterwards, so this snapshot is `after_delete`'s only view of
    // what was removed.
    let hook_data = prepare_delete_hook_data(ctx, write_hooks, def, conn, id, locale_config)?;

    // Ref count protection (hard delete only).
    if !def.soft_delete {
        let ref_count = query::ref_count::get_ref_count_locked(conn, ctx.slug, id)?.unwrap_or(0);

        if ref_count > 0 {
            return Err(ServiceError::Referenced {
                id: id.to_string(),
                count: ref_count,
            });
        }
    }

    let hook_ctx = HookContext::builder(ctx.slug, "delete")
        .data(hook_data.clone())
        .document_id(id)
        .user(ctx.user)
        .build();

    let final_ctx =
        write_hooks.run_hooks_with_conn(&def.hooks, HookEvent::BeforeDelete, hook_ctx, conn)?;

    let locale_cfg = locale_config.cloned().unwrap_or_default();
    let purge_locale = LocaleContext::default_for(&locale_cfg);

    // The files this document owns — the published row's AND every version
    // snapshot's, so a file only a never-published draft ever named goes with
    // the document instead of staying behind forever. Read after the
    // before-hooks, so one that rewrote the row through its own CRUD is
    // accounted for, and before the delete removes both the row and its
    // versions. A soft delete resolves none: an undelete brings the document
    // back and must find its files.
    let upload_keys = if def.soft_delete {
        Vec::new()
    } else {
        owned_file_keys(conn, def, id, purge_locale.as_ref())?
    };

    let event = execute_delete(ctx, conn, id, &locale_cfg)?;

    // After-delete hooks
    let after_ctx = HookContext::builder(ctx.slug, "delete")
        .data(hook_data)
        .document_id(id)
        .context(final_ctx.context)
        .user(ctx.user)
        .build();

    let after_result =
        write_hooks.run_hooks_with_conn(&def.hooks, HookEvent::AfterDelete, after_ctx, conn)?;

    // Hard-deleting an auth document revokes that user's sessions, so its live
    // streams must be torn down — but the invalidation is published POST-COMMIT
    // by the wrapper (`delete_document_pool`/`_conn`, `delete_many_*`), mirroring
    // update/undelete so a rollback can't leave a phantom invalidation.

    Ok(DeleteResult {
        context: after_result.context,
        upload_keys,
        event,
    })
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use std::{
        cell::RefCell,
        rc::Rc,
        sync::{Arc, Mutex},
    };

    use rusqlite::Connection;
    use serde_json::json;

    use crate::{
        config::CrapConfig,
        core::{
            CollectionDefinition, FieldDefinition, FieldType, Hooks, JobStatus, Registry,
            SharedEventTransport, SharedInvalidationTransport, ValidationError,
            collection::Auth,
            event::{EventOperation, InProcessEventBus, InProcessInvalidationBus},
            upload::{
                CollectionUpload, ImageConvertJobData, SYSTEM_IMAGE_CONVERT_JOB,
                queue_image_conversion,
            },
        },
        db::{DbConnection, Filter, FilterClause, FilterOp, migrate, pool},
        hooks::ValidationCtx,
        service::{
            DeleteManyOptions, EventQueue, FieldReadStrip, ServiceContext, delete_document,
            delete_many, hooks::WriteHooks,
        },
    };

    use super::*;

    /// Allow-all hooks that do not run any user-defined Lua.
    struct AllowAllWriteHooks;

    impl WriteHooks for AllowAllWriteHooks {
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
            _event: HookEvent,
            ctx: HookContext,
            _conn: &dyn DbConnection,
        ) -> anyhow::Result<HookContext> {
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

    impl FieldReadStrip for AllowAllWriteHooks {}

    fn setup_auth_collection() -> (Connection, CollectionDefinition) {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE users (
                id TEXT PRIMARY KEY,
                email TEXT,
                _ref_count INTEGER DEFAULT 0,
                created_at TEXT,
                updated_at TEXT
            );
            INSERT INTO users (id, email) VALUES ('u1', 'a@b.com');",
        )
        .unwrap();

        let mut def = CollectionDefinition::new("users");
        def.timestamps = true;
        def.fields = vec![
            FieldDefinition::builder("email", FieldType::Email)
                .unique(true)
                .build(),
        ];
        def.auth = Some(Auth {
            enabled: true,
            ..Default::default()
        });

        (conn, def)
    }

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

    /// Regression: a soft delete cancelled the upload's queued image
    /// conversions and a restore never re-queued them, so an upload trashed
    /// before its conversions ran came back without its variant URLs.
    #[test]
    fn a_soft_delete_keeps_queued_image_conversions() {
        let mut media = CollectionDefinition::new("media");
        media.soft_delete = true;
        media.upload = Some(CollectionUpload {
            enabled: true,
            ..Default::default()
        });

        let tmp = tempfile::tempdir().expect("tempdir");
        let mut config = CrapConfig::test_default();
        config.database.path = "test.db".to_string();
        let db_pool = pool::create_pool(tmp.path(), &config).expect("pool");
        let shared = Registry::shared();
        shared.write().unwrap().register_collection(media.clone());
        migrate::sync_all(&db_pool, &Registry::snapshot(&shared), &config.locale).expect("sync");

        let conn = db_pool.get().unwrap();
        conn.execute("INSERT INTO media (id) VALUES ('m1')", &[])
            .unwrap();
        let job = ImageConvertJobData {
            collection: "media".to_string(),
            document_id: "m1".to_string(),
            source_path: "a.png".to_string(),
            target_path: "a.webp".to_string(),
            format: "webp".to_string(),
            quality: 80,
            url_column: "thumbnail_webp_url".to_string(),
            url_value: "/uploads/a.webp".to_string(),
        };
        queue_image_conversion(&conn, &job, 1).unwrap();

        let hooks = AllowAllWriteHooks;
        let ctx = ServiceContext::collection("media", &media)
            .conn(&conn)
            .write_hooks(&hooks)
            .override_access(true)
            .build();

        delete_document_in_conn(&ctx, "m1", None).expect("soft delete");

        let pending = query::jobs::count_job_runs(
            &conn,
            Some(SYSTEM_IMAGE_CONVERT_JOB),
            Some(JobStatus::Pending),
        )
        .unwrap();
        assert_eq!(pending, 1, "a trashed upload keeps its queued conversion");
    }

    /// A `posts` table holding `p1` (owned by `u1`) and `p2` (owned by `u2`);
    /// soft-deleting when `soft_delete`.
    fn owned_posts(soft_delete: bool) -> (Connection, CollectionDefinition) {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                owner TEXT,
                _ref_count INTEGER DEFAULT 0,
                _deleted_at TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            INSERT INTO posts (id, owner) VALUES ('p1', 'u1'), ('p2', 'u2');",
        )
        .unwrap();

        let mut def = CollectionDefinition::new("posts");
        def.timestamps = true;
        def.soft_delete = soft_delete;
        def.fields = vec![FieldDefinition::builder("owner", FieldType::Text).build()];

        (conn, def)
    }

    fn owner_is(owner: &str) -> [FilterClause; 1] {
        [FilterClause::Single(Filter {
            field: "owner".to_string(),
            op: FilterOp::Equals(owner.to_string()),
        })]
    }

    /// A conn-mode context that publishes its events into `queue`.
    fn publishing_ctx<'a>(
        conn: &'a Connection,
        def: &'a CollectionDefinition,
        hooks: &'a AllowAllWriteHooks,
        queue: &EventQueue,
    ) -> ServiceContext<'a> {
        let transport: SharedEventTransport = Arc::new(InProcessEventBus::new(16));

        ServiceContext::collection("posts", def)
            .conn(conn)
            .write_hooks(hooks)
            .override_access(true)
            .event_transport(Some(transport))
            .event_queue(queue.clone())
            .build()
    }

    /// Regression: a delete event carried no document, so a subscriber with a
    /// row-constrained view never learned that a row it could see was deleted.
    /// A hard delete now carries the row as read just before it was removed.
    #[test]
    fn hard_delete_event_carries_the_removed_row() {
        let (conn, def) = owned_posts(false);
        let hooks = AllowAllWriteHooks;
        let queue: EventQueue = Rc::new(RefCell::new(Vec::new()));
        let ctx = publishing_ctx(&conn, &def, &hooks, &queue);

        delete_document(&ctx, "p1", None, None).expect("delete");

        let queued = queue.borrow();
        let event = queued.first().expect("delete event queued");
        let gate = event.gate.as_ref().expect("the removed row rides along");

        assert_eq!(event.operation, EventOperation::Delete);
        assert!(event.data.is_empty(), "a delete delivers no document");
        assert!(!event.view.trashed);
        assert!(gate.matches(&owner_is("u1"), &def.fields));
        assert!(!gate.matches(&owner_is("u2"), &def.fields));
    }

    /// A soft delete carries the row as it now sits in the trash — the view the
    /// event is gated by — so a trash-view constraint is judged like SQL's.
    #[test]
    fn soft_delete_event_carries_the_trashed_row() {
        let (conn, def) = owned_posts(true);
        let hooks = AllowAllWriteHooks;
        let queue: EventQueue = Rc::new(RefCell::new(Vec::new()));
        let ctx = publishing_ctx(&conn, &def, &hooks, &queue);

        delete_document(&ctx, "p1", None, None).expect("soft delete");

        let queued = queue.borrow();
        let event = queued.first().expect("delete event queued");
        let gate = event.gate.as_ref().expect("the trashed row rides along");
        let trashed = [FilterClause::Single(Filter {
            field: "_deleted_at".to_string(),
            op: FilterOp::Exists,
        })];

        assert!(event.view.trashed);
        assert!(gate.matches(&owner_is("u1"), &def.fields));
        assert!(gate.matches(&trashed, &def.fields));
    }

    /// A delete that publishes no event reads nothing for it.
    #[test]
    fn delete_without_events_reads_no_event_row() {
        let (conn, def) = owned_posts(false);
        let hooks = AllowAllWriteHooks;
        let ctx = ServiceContext::collection("posts", &def)
            .conn(&conn)
            .write_hooks(&hooks)
            .override_access(true)
            .build();

        let result = delete_document_in_conn(&ctx, "p1", None).expect("delete");

        assert!(result.event.is_none());
    }

    /// Regression: a hard delete of a trashed row — a forced delete, or a
    /// purge of the trash — runs on the collection's hard-delete variant,
    /// which reads no trash column, so its event was gated by the row's status
    /// view: a subscriber without trash access learned of a document it could
    /// no longer see. It is gated by the trash, the view the row was last in,
    /// and carries the trash timestamp for a trash-view constraint.
    #[test]
    fn a_hard_delete_of_a_trashed_row_is_gated_by_the_trash() {
        let (conn, soft) = owned_posts(true);
        query::soft_delete(&conn, "posts", "p1").unwrap();

        let mut hard = soft.clone();
        hard.make_hard_delete();

        let hooks = AllowAllWriteHooks;
        let queue: EventQueue = Rc::new(RefCell::new(Vec::new()));
        let ctx = publishing_ctx(&conn, &hard, &hooks, &queue);

        delete_document(&ctx, "p1", None, None).expect("delete the trashed row");
        delete_document(&ctx, "p2", None, None).expect("delete the live row");

        let queued = queue.borrow();
        let trashed = [FilterClause::Single(Filter {
            field: "_deleted_at".to_string(),
            op: FilterOp::Exists,
        })];

        let purged = queued.first().expect("the trashed row's event");
        let gate = purged.gate.as_ref().expect("the removed row rides along");
        assert!(purged.view.trashed, "a trashed row is gated by the trash");
        assert!(gate.matches(&trashed, &hard.fields));
        assert!(gate.matches(&owner_is("u1"), &hard.fields));

        let live = queued.get(1).expect("the live row's event");
        assert!(!live.view.trashed, "a live row keeps its status view");
    }

    /// Every document a bulk delete removes carries its own row.
    #[test]
    fn bulk_delete_events_carry_each_removed_row() {
        let (conn, def) = owned_posts(false);
        let hooks = AllowAllWriteHooks;
        let queue: EventQueue = Rc::new(RefCell::new(Vec::new()));
        let ctx = publishing_ctx(&conn, &def, &hooks, &queue);
        let both = [FilterClause::Single(Filter {
            field: "owner".to_string(),
            op: FilterOp::In(vec!["u1".to_string(), "u2".to_string()]),
        })];

        delete_many(
            &ctx,
            &both,
            &LocaleConfig::default(),
            &DeleteManyOptions::default(),
        )
        .expect("bulk delete");

        let queued = queue.borrow();
        assert_eq!(queued.len(), 2);

        for event in queued.iter() {
            let owner = if event.document_id == "p1" {
                "u1"
            } else {
                "u2"
            };
            let gate = event.gate.as_ref().expect("each removed row rides along");

            assert!(
                gate.matches(&owner_is(owner), &def.fields),
                "{}",
                event.document_id
            );
        }
    }
}
