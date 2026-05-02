//! Core delete operation for collections.

use std::collections::HashMap;

use serde_json::Value;

use crate::{
    config::LocaleConfig,
    db::{AccessResult, LocaleContext, query},
    hooks::{HookContext, HookEvent},
    service::{ServiceContext, helpers::enforce_access_constraints},
};

use super::ServiceError;

type Result<T> = std::result::Result<T, ServiceError>;

/// Result of a delete operation.
pub struct DeleteResult {
    /// Request-scoped context returned by after-delete hooks.
    pub context: HashMap<String, Value>,
    /// Upload file fields from the deleted document (for post-commit cleanup).
    pub upload_doc_fields: Option<HashMap<String, Value>>,
}

/// Delete a document on an existing connection/transaction.
///
/// Runs the full lifecycle: ref count check -> before-delete hooks -> delete -> cleanup -> after-delete hooks.
/// Does NOT manage transactions — caller must open/commit.
/// Upload file cleanup is returned as `upload_doc_fields` for the caller to handle after commit.
pub fn delete_document_core(
    ctx: &ServiceContext,
    id: &str,
    locale_config: Option<&LocaleConfig>,
) -> Result<DeleteResult> {
    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();
    let write_hooks = ctx.write_hooks()?;
    let def = ctx.collection_def();

    // Collection-level access check — use trash access for soft delete, delete for hard
    let access_ref = if def.soft_delete {
        def.access.resolve_trash()
    } else {
        def.access.delete.as_deref()
    };

    let access = write_hooks.check_access(access_ref, ctx.user, Some(id), None)?;

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

    // Pre-load upload doc for file cleanup (before deletion removes it)
    let upload_doc_fields = if def.is_upload_collection() {
        let lc = locale_config.cloned().unwrap_or_default();
        let locale_ctx = LocaleContext::from_locale_string(None, &lc)?;

        query::find_by_id(conn, ctx.slug, def, id, locale_ctx.as_ref())
            .ok()
            .flatten()
            .map(|d| d.fields.clone())
    } else {
        None
    };

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

    // Before-delete hooks
    let mut hook_data: HashMap<String, Value> =
        [("id".to_string(), Value::String(id.to_string()))].into();

    if def.soft_delete {
        hook_data.insert("soft_delete".to_string(), Value::Bool(true));
    }

    let hook_ctx = HookContext::builder(ctx.slug, "delete")
        .data(hook_data.clone())
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
        let _ = query::images::delete_entries_for_document(conn, ctx.slug, id);
    }

    // After-delete hooks
    let after_ctx = HookContext::builder(ctx.slug, "delete")
        .data(hook_data)
        .context(final_ctx.context)
        .user(ctx.user)
        .build();

    let after_result =
        write_hooks.run_hooks_with_conn(&def.hooks, HookEvent::AfterDelete, after_ctx, conn)?;

    // Hard-deleting an auth document revokes that user's sessions — tear
    // down any active live-update streams. Soft delete preserves the row,
    // so no tear-down. No-op when no invalidation transport is attached.
    if !def.soft_delete && def.is_auth_collection() {
        ctx.publish_user_invalidation(id);
    }

    Ok(DeleteResult {
        context: after_result.context,
        upload_doc_fields,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rusqlite::Connection;

    use crate::{
        core::{
            CollectionDefinition, Document, FieldDefinition,
            collection::{Auth, Hooks},
            event::{InProcessInvalidationBus, SharedInvalidationTransport},
            field::FieldType,
            validate::ValidationError,
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
        ) -> Vec<String> {
            Vec::new()
        }

        fn check_access(
            &self,
            _access_ref: Option<&str>,
            _user: Option<&Document>,
            _id: Option<&str>,
            _data: Option<&HashMap<String, Value>>,
        ) -> anyhow::Result<AccessResult> {
            Ok(AccessResult::Allowed)
        }

        fn field_write_denied(
            &self,
            _fields: &[FieldDefinition],
            _user: Option<&Document>,
            _operation: &str,
        ) -> Vec<String> {
            Vec::new()
        }

        fn validate_fields(
            &self,
            _fields: &[FieldDefinition],
            _data: &HashMap<String, Value>,
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

        let _ = delete_document_core(&ctx, "u1", None).expect("delete");

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

        let _ = delete_document_core(&ctx, "u1", None).expect("soft delete");

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

        let _ = delete_document_core(&ctx, "p1", None).expect("delete");

        let recv_result =
            tokio::time::timeout(std::time::Duration::from_millis(150), rx.recv()).await;
        assert!(
            recv_result.is_err(),
            "non-auth hard-delete must not publish an invalidation signal"
        );
    }
}
