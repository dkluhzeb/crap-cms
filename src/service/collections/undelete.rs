//! Collection document undelete from soft-delete.

use crate::{
    core::{Document, event::EventOperation},
    db::{AccessResult, LocaleContext, query},
    hooks::AccessCheckInput,
    service::{
        Gated, ServiceContext, ServiceError, StateChange, helpers, invalidate_user_streams_if_auth,
        run_after_change_hooks, run_pool_write, run_state_before_change,
    },
};

type Result<T> = std::result::Result<T, ServiceError>;

/// The capability and access gates every undelete passes, whatever surface it
/// came from.
///
/// Soft-delete must be enabled (otherwise there is no trashed row to restore),
/// the `trash` access rule must allow the caller, and a `Constrained` result is
/// enforced against the target row — searched in the trash view, where it sits.
fn gate_undelete(ctx: &ServiceContext, id: &str) -> Result<()> {
    let write_hooks = ctx.write_hooks()?;
    let def = ctx.collection_def()?;

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

    helpers::enforce_access_constraints(ctx, id, &access, "Undelete", true)
}

/// The trashed document as it stands going in: the data `before_change` sees.
///
/// A live row is refused here, before any hook runs: an undelete of it would
/// only fail once `before_change` had already acted on it. Read with
/// `include_deleted`, since the row is in the trash — and under the default
/// locale context, because a localized collection's per-locale columns
/// (`title__en`) only resolve with one.
fn trashed_document(
    ctx: &ServiceContext,
    id: &str,
    locale_ctx: Option<&LocaleContext>,
) -> Result<Document> {
    let conn = ctx.resolve_conn()?;
    let def = ctx.collection_def()?;
    let not_trashed = || ServiceError::NotFound("Document not found or not deleted".into());

    let trashed = query::stored_deleted_at(conn.as_ref(), ctx.slug, id)?
        .is_some_and(|deleted_at| !deleted_at.is_null());

    if !trashed {
        return Err(not_trashed());
    }

    query::find_by_id_raw(conn.as_ref(), ctx.slug, def, id, locale_ctx, true)?
        .ok_or_else(not_trashed)
}

/// Core undelete logic on an existing connection: gates, `before_change`,
/// restore the row, `after_change`. Returns the stored row the undelete event
/// is built from alongside the document.
///
/// Does NOT manage transactions — caller must open/commit.
fn undelete_document_in_conn(ctx: &ServiceContext, id: &str) -> Result<Gated<Document>> {
    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();
    let write_hooks = ctx.write_hooks()?;
    let def = ctx.collection_def()?;

    gate_undelete(ctx, id)?;

    let locale_ctx = ctx.default_locale_ctx();
    let change = StateChange::Undelete;

    let trashed = trashed_document(ctx, id, locale_ctx.as_ref())?;
    let req_context = run_state_before_change(ctx, change, &trashed, locale_ctx.as_ref())?;

    // A soft-deleted row keeps its FTS entry (the trash view is searchable),
    // so nothing to re-index.
    if !query::restore(conn, ctx.slug, id)? {
        return Err(ServiceError::NotFound(
            "Document not found or not deleted".into(),
        ));
    }

    let mut doc = query::find_by_id(conn, ctx.slug, def, id, locale_ctx.as_ref())?
        .ok_or_else(|| ServiceError::NotFound("Document not found after undelete".into()))?;

    run_after_change_hooks(
        write_hooks,
        &def.hooks,
        &def.fields,
        &doc,
        change.after_change(ctx, locale_ctx.as_ref(), req_context),
        conn,
    )?;

    // The row as stored, before anything is shaped or stripped for the writer:
    // the live event is built from it.
    let row = ctx.event_row(&doc);

    helpers::strip_reported(ctx, write_hooks, &mut doc, locale_ctx.as_ref())?;

    Ok((doc, row))
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
    let (doc, _) = run_pool_write(
        ctx,
        None,
        |inner| undelete_document_in_conn(inner, id),
        |ctx, (doc, row)| {
            ctx.publish_mutation_event(EventOperation::Undelete, &doc.id, row.clone());
            // Restoring an auth document changes that user's effective access.
            invalidate_user_streams_if_auth(ctx, &doc.id);
        },
    )?;

    Ok(doc)
}

/// Conn-based undelete: uses existing connection (Lua CRUD path).
fn undelete_document_conn(ctx: &ServiceContext, id: &str) -> Result<Document> {
    let (doc, row) = undelete_document_in_conn(ctx, id)?;

    ctx.clear_cache();

    ctx.publish_mutation_event(EventOperation::Undelete, &doc.id, row);
    invalidate_user_streams_if_auth(ctx, &doc.id);

    Ok(doc)
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use std::{cell::RefCell, collections::HashMap};

    use anyhow::{Result as AnyResult, anyhow};
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

    /// Write hooks that record the operation of every lifecycle event they are
    /// handed, and can be told to fail the `before_change` one.
    struct RecordingWriteHooks {
        before: RefCell<Vec<String>>,
        after: RefCell<Vec<String>>,
        fail_before: bool,
    }

    impl RecordingWriteHooks {
        fn new(fail_before: bool) -> Self {
            Self {
                before: RefCell::new(Vec::new()),
                after: RefCell::new(Vec::new()),
                fail_before,
            }
        }
    }

    impl WriteHooks for RecordingWriteHooks {
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
            self.after.borrow_mut().push(ctx.operation.clone());
            Ok(ctx)
        }

        fn run_hooks_with_conn(
            &self,
            _hooks: &Hooks,
            _event: HookEvent,
            ctx: HookContext,
            _conn: &dyn DbConnection,
        ) -> AnyResult<HookContext> {
            self.before.borrow_mut().push(ctx.operation.clone());

            if self.fail_before {
                return Err(anyhow!("before_change refused the undelete"));
            }

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

    impl FieldReadStrip for RecordingWriteHooks {}

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

        let (doc, _) = undelete_document_in_conn(&ctx, &id).unwrap();

        let items = doc.fields.get("items").and_then(Value::as_array);
        assert_eq!(
            items.map(Vec::len),
            Some(1),
            "undelete must report the array rows: {:?}",
            doc.fields
        );
    }

    /// Regression: undelete ran no lifecycle hooks at all, so a collection
    /// could not react to a document coming back out of the trash — while
    /// unpublish, its sibling state write, ran the full pair. Both events name
    /// the operation `undelete`, matching the event the write publishes.
    #[test]
    fn undelete_runs_the_lifecycle_hooks() {
        let (_tmp, db_pool, def, id) = trashed_document_with_array_row();
        let conn = db_pool.get().unwrap();
        let hooks = RecordingWriteHooks::new(false);
        let ctx = ServiceContext::collection("articles", &def)
            .conn(&conn)
            .write_hooks(&hooks)
            .build();

        undelete_document_in_conn(&ctx, &id).unwrap();

        assert_eq!(hooks.before.borrow().join(","), "undelete");
        assert_eq!(hooks.after.borrow().join(","), "undelete");
    }

    /// Regression: undelete read its target with the trash included but never
    /// checked the row was trashed, so undeleting a live document ran
    /// `before_change` — and its side effects — before failing.
    #[test]
    fn undeleting_a_live_document_runs_no_hook() {
        let (_tmp, db_pool, def, id) = trashed_document_with_array_row();
        let conn = db_pool.get().unwrap();
        query::restore(&conn, "articles", &id).unwrap();

        let hooks = RecordingWriteHooks::new(false);
        let ctx = ServiceContext::collection("articles", &def)
            .conn(&conn)
            .write_hooks(&hooks)
            .build();

        assert!(matches!(
            undelete_document_in_conn(&ctx, &id),
            Err(ServiceError::NotFound(_))
        ));
        assert!(
            hooks.before.borrow().is_empty(),
            "before_change must not run for a live document"
        );
    }

    /// A `before_change` hook that errors aborts the undelete: the row stays
    /// trashed and no `after_change` hook runs.
    #[test]
    fn a_failing_before_change_leaves_the_row_trashed() {
        let (_tmp, db_pool, def, id) = trashed_document_with_array_row();
        let conn = db_pool.get().unwrap();
        let hooks = RecordingWriteHooks::new(true);
        let ctx = ServiceContext::collection("articles", &def)
            .conn(&conn)
            .write_hooks(&hooks)
            .build();

        assert!(
            undelete_document_in_conn(&ctx, &id).is_err(),
            "a refusing before_change must fail the undelete"
        );
        assert!(
            hooks.after.borrow().is_empty(),
            "after_change must not run for a refused undelete"
        );
        assert!(
            query::find_by_id(&conn, "articles", &def, &id, None)
                .unwrap()
                .is_none(),
            "the document must still be in the trash"
        );
    }
}
