//! Collection document undelete from soft-delete.

use anyhow::Context as _;

use std::{cell::RefCell, rc::Rc};

use crate::{
    core::{Document, event::EventOperation},
    db::{AccessResult, query},
    hooks::{AccessCheckInput, LuaCrudInfra},
    service::{
        RunnerWriteHooks, ServiceContext, ServiceError, flush_queue, helpers,
        invalidate_user_streams_if_auth,
    },
};

type Result<T> = std::result::Result<T, ServiceError>;

/// Core undelete logic on an existing connection: access check + restore row + FTS re-sync.
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

    if conn.supports_fts()
        && let Ok(Some(doc)) = query::find_by_id_unfiltered(conn, ctx.slug, def, id, None)
    {
        query::fts::fts_upsert(conn, ctx.slug, &doc, Some(def))?;
    }

    let mut doc = query::find_by_id(conn, ctx.slug, def, id, None)?
        .ok_or_else(|| ServiceError::NotFound("Document not found after undelete".into()))?;

    write_hooks.strip_read_access_doc(&def.fields, &mut doc, ctx.slug, ctx.user, None);
    doc.strip_fields(&helpers::collect_api_hidden_field_names(&def.fields, ""));

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
    let pool = ctx.pool.context("pool required")?;
    let runner = ctx.runner()?;
    let def = ctx.collection_def()?;
    let mut conn = pool.write().context("DB connection")?;
    let tx = conn.transaction_immediate().context("Start transaction")?;

    let queue = Rc::new(RefCell::new(Vec::new()));

    let infra = LuaCrudInfra::from_ctx(ctx, Some(queue.clone()), None);

    let mut wh = RunnerWriteHooks::new(runner)
        .with_conn(&tx)
        .with_infra(infra);

    if ctx.override_access {
        wh = wh.with_override_access();
    }

    let inner_ctx = ServiceContext::collection(ctx.slug, def)
        .conn(&tx)
        .write_hooks(&wh)
        .inherit_write_infra(ctx)
        .event_queue(queue.clone())
        .build();

    let doc = undelete_document_in_conn(&inner_ctx, id)?;
    drop(inner_ctx);

    tx.commit()?;

    ctx.clear_cache();

    ctx.publish_mutation_event(EventOperation::Update, &doc.id, &doc.fields);
    invalidate_user_streams_if_auth(ctx, &doc.id);
    flush_queue(ctx, &queue);

    Ok(doc)
}

/// Conn-based undelete: uses existing connection (Lua CRUD path).
fn undelete_document_conn(ctx: &ServiceContext, id: &str) -> Result<Document> {
    let doc = undelete_document_in_conn(ctx, id)?;

    ctx.clear_cache();

    ctx.publish_mutation_event(EventOperation::Update, &doc.id, &doc.fields);
    invalidate_user_streams_if_auth(ctx, &doc.id);

    Ok(doc)
}
