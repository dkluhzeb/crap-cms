//! Collection document undelete from soft-delete.

use anyhow::Context as _;

use crate::{
    core::Document,
    db::{AccessResult, query},
    service::{RunnerWriteHooks, ServiceContext, ServiceError, helpers},
};

type Result<T> = std::result::Result<T, ServiceError>;

/// Core undelete logic on an existing connection: access check + restore row + FTS re-sync.
///
/// Does NOT manage transactions — caller must open/commit.
pub fn undelete_document_core(ctx: &ServiceContext, id: &str) -> Result<Document> {
    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();
    let write_hooks = ctx.write_hooks()?;
    let def = ctx.collection_def();

    let access = write_hooks.check_access(def.access.resolve_trash(), ctx.user, Some(id), None)?;

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

    let mut read_denied = write_hooks.field_read_denied(&def.fields, ctx.user);
    read_denied.extend(helpers::collect_hidden_field_names(&def.fields, ""));

    doc.strip_fields(&read_denied);

    Ok(doc)
}

/// Undelete a soft-deleted document within a single transaction.
#[cfg(not(tarpaulin_include))]
pub fn undelete_document(ctx: &ServiceContext, id: &str) -> Result<Document> {
    let pool = ctx.pool.context("pool required")?;
    let runner = ctx.runner()?;
    let def = ctx.collection_def();
    let mut conn = pool.get().context("DB connection")?;
    let tx = conn.transaction_immediate().context("Start transaction")?;

    let mut wh = RunnerWriteHooks::new(runner).with_conn(&tx);

    if ctx.override_access {
        wh = wh.with_override_access();
    }

    let inner_ctx = ServiceContext::collection(ctx.slug, def)
        .conn(&tx)
        .write_hooks(&wh)
        .user(ctx.user)
        .override_access(ctx.override_access)
        .build();

    let doc = undelete_document_core(&inner_ctx, id)?;

    tx.commit()?;

    Ok(doc)
}
