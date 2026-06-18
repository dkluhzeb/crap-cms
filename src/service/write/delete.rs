//! Core delete operation for collections.

use serde_json::Value;

use crate::{
    config::LocaleConfig,
    core::{CollectionDefinition, DocumentFields, ReqContext},
    db::{AccessResult, DbConnection, LocaleContext, query},
    hooks::{AccessCheckInput, HookContext, HookEvent},
    service::{ServiceContext, helpers::enforce_access_constraints, hooks::WriteHooks},
};

use super::ServiceError;

type Result<T> = std::result::Result<T, ServiceError>;

/// Result of a delete operation.
pub(crate) struct DeleteResult {
    /// Request-scoped context returned by after-delete hooks.
    pub context: ReqContext,
    /// Upload file fields from the deleted document (for post-commit cleanup).
    pub upload_doc_fields: Option<DocumentFields>,
    /// The document's `_status` before deletion (draft collections only), used
    /// to gate a hard-delete event by the status view it was last in. `None`
    /// for status-less collections and soft-deletes (gated by `trash`).
    pub pre_status: Option<String>,
}

/// Load the document's fields once (before deletion removes the row) and build
/// the delete-hook `data`. Returns `(upload_doc_fields, hook_data, pre_status)`:
///
/// - `upload_doc_fields` — fields for post-commit upload-file cleanup (only for
///   upload collections; `None` otherwise).
/// - `hook_data` — the `data` passed to `before_delete` / `after_delete`: the
///   document's full fields (when a delete hook will run) plus `id`, and
///   `soft_delete` for a soft delete; otherwise just `{ id }` (+ `soft_delete`).
/// - `pre_status` — the document's `_status` before deletion, for the mutation
///   event's view gate (draft collections only; `None` otherwise).
///
/// The document is loaded only when an upload collection, a delete hook, or a
/// status axis needs it, so a plain delete on a status-less, hook-less
/// collection does no extra query.
fn prepare_delete_hook_data(
    ctx: &ServiceContext,
    write_hooks: &dyn WriteHooks,
    def: &CollectionDefinition,
    conn: &dyn DbConnection,
    id: &str,
    locale_config: Option<&LocaleConfig>,
) -> Result<(Option<DocumentFields>, DocumentFields, Option<String>)> {
    let wants_hook_data = write_hooks.runs_delete_hooks(&def.hooks);

    let doc_fields = if def.is_upload_collection() || wants_hook_data || def.has_drafts() {
        let lc = locale_config.cloned().unwrap_or_default();
        let locale_ctx = LocaleContext::from_locale_string(None, &lc)?;

        query::find_by_id(conn, ctx.slug, def, id, locale_ctx.as_ref())
            .ok()
            .flatten()
            .map(|d| d.fields)
    } else {
        None
    };

    let upload_doc_fields = if def.is_upload_collection() {
        doc_fields.clone()
    } else {
        None
    };

    // Capture `_status` before `doc_fields` is consumed into `hook_data`.
    let pre_status = doc_fields
        .as_ref()
        .and_then(|f| f.get_str("_status").map(str::to_string));

    let mut hook_data = if wants_hook_data {
        doc_fields.unwrap_or_default()
    } else {
        DocumentFields::new()
    };
    hook_data.insert("id".to_string(), Value::String(id.to_string()));

    if def.soft_delete {
        hook_data.insert("soft_delete".to_string(), Value::Bool(true));
    }

    Ok((upload_doc_fields, hook_data, pre_status))
}

/// Delete a document on an existing connection/transaction.
///
/// Runs the full lifecycle: ref count check -> before-delete hooks -> delete -> cleanup -> after-delete hooks.
/// Does NOT manage transactions — caller must open/commit.
/// Upload file cleanup is returned as `upload_doc_fields` for the caller to handle after commit.
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
    let access = write_hooks.check_access(&AccessCheckInput {
        access: access_ref,
        user: ctx.user,
        id: Some(id),
        data: None,
        locale: None,
        // A soft delete is a "trash" operation (gated by the trash access fn);
        // a hard delete is "delete". Keeps the operation label consistent with
        // the access fn being invoked (and with the admin permission grid).
        operation: if def.soft_delete { "trash" } else { "delete" },
        collection: ctx.slug,
        ui_locale: None,
    })?;

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

    // Load the document fields once (before deletion removes the row) for upload
    // cleanup and the delete-hook context, and build the hook `data`. For a hard
    // delete the row is gone afterwards, so this snapshot is `after_delete`'s
    // only view of what was removed.
    let (upload_doc_fields, hook_data, pre_status) =
        prepare_delete_hook_data(ctx, write_hooks, def, conn, id, locale_config)?;

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

    // Decrement ref counts before hard delete
    if !def.soft_delete {
        let locale_cfg = locale_config.cloned().unwrap_or_default();

        query::ref_count::before_hard_delete(conn, ctx.slug, id, &def.fields, &locale_cfg)?;
    }

    // Execute delete
    if def.soft_delete {
        let deleted = query::soft_delete(conn, ctx.slug, id)?;

        if !deleted {
            return Err(ServiceError::NotFound(format!(
                "Document '{id}' not found in '{}' (or already deleted)",
                ctx.slug
            )));
        }
    } else {
        let deleted = query::delete(conn, ctx.slug, id)?;

        if !deleted {
            return Err(ServiceError::NotFound(format!(
                "Document '{id}' not found in '{}'",
                ctx.slug
            )));
        }
    }

    // Cleanup
    if conn.supports_fts() {
        query::fts::fts_delete(conn, ctx.slug, id)?;
    }
    if def.is_upload_collection() {
        // Best-effort cleanup of any queued `_system_image_convert`
        // jobs targeting this doc — see `core/upload/queue.rs`.
        let _ = crate::core::upload::delete_image_jobs_for_document(conn, ctx.slug, id);
    }

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
        upload_doc_fields,
        pre_status,
    })
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use std::sync::{Arc, Mutex};

    use rusqlite::Connection;
    use serde_json::json;

    use crate::{
        core::{
            CollectionDefinition, Document, FieldDefinition, FieldDenial, FieldType, Hooks,
            SharedInvalidationTransport, ValidationError, collection::Auth,
            event::InProcessInvalidationBus,
        },
        db::DbConnection,
        hooks::ValidationCtx,
        service::{ServiceContext, hooks::WriteHooks},
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

        fn field_read_denied(
            &self,
            _fields: &[FieldDefinition],
            _user: Option<&Document>,
            _locale: Option<&str>,
        ) -> Vec<FieldDenial> {
            Vec::new()
        }

        fn check_access(&self, _input: &AccessCheckInput<'_>) -> anyhow::Result<AccessResult> {
            Ok(AccessResult::Allowed)
        }

        fn field_write_denied(
            &self,
            _fields: &[FieldDefinition],
            _user: Option<&Document>,
            _locale: Option<&str>,
            _operation: &str,
        ) -> Vec<FieldDenial> {
            Vec::new()
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
        let _ = crate::service::delete_document(&ctx, "u1", None, None).expect("delete");

        let received = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .expect("recv timed out")
            .expect("expected invalidation signal");
        assert_eq!(received, "u1");
    }

    #[tokio::test]
    async fn soft_delete_auth_does_not_publish() {
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

        let _ = crate::service::delete_document(&ctx, "u1", None, None).expect("soft delete");

        // No publish must have happened — poll briefly and assert timeout.
        let recv_result =
            tokio::time::timeout(std::time::Duration::from_millis(150), rx.recv()).await;
        assert!(
            recv_result.is_err(),
            "soft-delete must not publish an invalidation signal"
        );
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

        fn field_read_denied(
            &self,
            _fields: &[FieldDefinition],
            _user: Option<&Document>,
            _locale: Option<&str>,
        ) -> Vec<FieldDenial> {
            Vec::new()
        }

        fn check_access(&self, _input: &AccessCheckInput<'_>) -> anyhow::Result<AccessResult> {
            Ok(AccessResult::Allowed)
        }

        fn field_write_denied(
            &self,
            _fields: &[FieldDefinition],
            _user: Option<&Document>,
            _locale: Option<&str>,
            _operation: &str,
        ) -> Vec<FieldDenial> {
            Vec::new()
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
}
