//! Collection document undelete from soft-delete.

use crate::{
    core::{Document, event::EventOperation},
    db::{AccessResult, query},
    hooks::AccessCheckInput,
    service::{
        ServiceContext, ServiceError, helpers, invalidate_user_streams_if_auth, run_pool_write,
    },
};

type Result<T> = std::result::Result<T, ServiceError>;

/// Core undelete logic on an existing connection: access check + restore row.
///
/// Does NOT manage transactions — caller must open/commit.
fn undelete_document_in_conn(ctx: &ServiceContext, id: &str) -> Result<Document> {
    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();
    let write_hooks = ctx.write_hooks()?;
    let def = ctx.collection_def()?;

    // Authoritative capability gate: undelete only makes sense when soft-delete is
    // enabled (otherwise there is no trashed row to restore). Enforced at the one
    // service chokepoint so every surface agrees.
    if !def.has_soft_delete() {
        return Err(ServiceError::HookError(format!(
            "Collection '{}' does not support undelete: soft-delete is not enabled",
            ctx.slug
        )));
    }

    let access = write_hooks.check_access(
        &AccessCheckInput::builder("undelete", ctx.slug)
            .access(def.access.resolve_trash())
            .user(ctx.user)
            .id(Some(id))
            .build(),
    )?;

    if matches!(access, AccessResult::Denied) {
        return Err(ServiceError::AccessDenied("Undelete access denied".into()));
    }

    // When the hook returned Constrained filters, enforce row-level match.
    // The target row is soft-deleted, so we must search the trash view.
    helpers::enforce_access_constraints(ctx, id, &access, "Undelete", true)?;

    let restored = query::restore(conn, ctx.slug, id)?;

    if !restored {
        return Err(ServiceError::NotFound(
            "Document not found or not deleted".into(),
        ));
    }

    // A soft-deleted row keeps its FTS entry (the trash view is searchable),
    // so nothing to re-index. Re-read under the default locale context: a
    // localized collection's per-locale columns (`title__en`) only resolve with
    // one; the bare `title` column does not exist there.
    let locale_ctx = ctx.default_locale_ctx();
    let mut doc = query::find_by_id(conn, ctx.slug, def, id, locale_ctx.as_ref())?
        .ok_or_else(|| ServiceError::NotFound("Document not found after undelete".into()))?;

    helpers::strip_reported(ctx, write_hooks, &mut doc, locale_ctx.as_ref())?;

    Ok(doc)
}

/// Undelete a soft-deleted document.
///
/// **Pool mode** (`ctx.pool` set): opens a transaction, commits after success.
/// **Conn mode** (`ctx.conn` set, Lua CRUD path): runs on the existing connection.
///
/// # Errors
///
/// Returns service-layer errors (access denied, document not found, hook
/// errors) or a backend error if the DB transaction or persistence fails.
#[cfg(not(tarpaulin_include))]
pub fn undelete_document(ctx: &ServiceContext, id: &str) -> Result<Document> {
    if ctx.pool.is_some() {
        undelete_document_pool(ctx, id)
    } else {
        undelete_document_conn(ctx, id)
    }
}

/// Pool-based undelete: own transaction with event publishing after commit.
fn undelete_document_pool(ctx: &ServiceContext, id: &str) -> Result<Document> {
    run_pool_write(
        ctx,
        None,
        |inner| undelete_document_in_conn(inner, id),
        |ctx, doc| {
            ctx.publish_mutation_event(EventOperation::Undelete, &doc.id, &doc.fields);
            // Restoring an auth document changes that user's effective access.
            invalidate_user_streams_if_auth(ctx, &doc.id);
        },
    )
}

/// Conn-based undelete: uses existing connection (Lua CRUD path).
fn undelete_document_conn(ctx: &ServiceContext, id: &str) -> Result<Document> {
    let doc = undelete_document_in_conn(ctx, id)?;

    ctx.clear_cache();

    ctx.publish_mutation_event(EventOperation::Undelete, &doc.id, &doc.fields);
    invalidate_user_streams_if_auth(ctx, &doc.id);

    Ok(doc)
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use std::collections::HashMap;

    use anyhow::Result as AnyResult;
    use serde_json::{Value, json};

    use super::*;
    use crate::{
        config::CrapConfig,
        core::{
            CollectionDefinition, DocumentFields, FieldDefinition, FieldType, Hooks, Registry,
            ValidationError,
        },
        db::{DbConnection, DbPool, migrate, pool},
        hooks::{HookContext, HookEvent, ValidationCtx},
        service::{FieldReadStrip, hooks::WriteHooks},
    };

    /// Write hooks that run nothing and allow every access check.
    struct AllowAllWriteHooks;

    impl WriteHooks for AllowAllWriteHooks {
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

    impl FieldReadStrip for AllowAllWriteHooks {}

    /// A soft-delete collection with an `items` array, holding one trashed
    /// document that has one array row. Returns the pool, definition and id.
    fn trashed_document_with_array_row() -> (tempfile::TempDir, DbPool, CollectionDefinition, String)
    {
        let mut def = CollectionDefinition::new("articles");
        def.timestamps = true;
        def.soft_delete = true;
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("label", FieldType::Text).build(),
                ])
                .build(),
        ];

        let tmp = tempfile::tempdir().unwrap();
        let mut config = CrapConfig::test_default();
        config.database.path = "test.db".to_string();
        let db_pool = pool::create_pool(tmp.path(), &config).unwrap();

        let shared = Registry::shared();
        shared.write().unwrap().register_collection(def.clone());
        migrate::sync_all(&db_pool, &shared.read().unwrap(), &config.locale).unwrap();

        let conn = db_pool.get().unwrap();
        let data: DocumentFields = [("title".to_string(), json!("Lazarus"))]
            .into_iter()
            .collect();
        let id = query::create(&conn, "articles", &def, &data, None)
            .unwrap()
            .id
            .to_string();

        let rows = [HashMap::from([("label".to_string(), json!("first"))])];
        query::set_array_rows(
            &conn,
            "articles",
            "items",
            &id,
            &rows,
            &def.fields[1].fields,
            None,
        )
        .unwrap();
        query::soft_delete(&conn, "articles", &id).unwrap();
        drop(conn);

        (tmp, db_pool, def, id)
    }

    /// Undelete reported the restored row without hydrating its join fields,
    /// so the response and the `Undelete` event lacked the array rows create,
    /// update, unpublish and restore all report.
    #[test]
    fn undelete_reports_the_restored_array_rows() {
        let (_tmp, db_pool, def, id) = trashed_document_with_array_row();
        let conn = db_pool.get().unwrap();
        let hooks = AllowAllWriteHooks;
        let ctx = ServiceContext::collection("articles", &def)
            .conn(&conn)
            .write_hooks(&hooks)
            .build();

        let doc = undelete_document_in_conn(&ctx, &id).unwrap();

        let items = doc.fields.get("items").and_then(Value::as_array);
        assert_eq!(
            items.map(Vec::len),
            Some(1),
            "undelete must report the array rows: {:?}",
            doc.fields
        );
    }
}
