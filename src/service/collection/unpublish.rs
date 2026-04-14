//! Collection document unpublish.

use anyhow::Context as _;
use serde_json::Value;

use crate::{
    core::Document,
    db::{AccessResult, query},
    hooks::{HookContext, HookEvent},
    service::{
        AfterChangeInput, RunnerWriteHooks, ServiceContext, ServiceError, helpers,
        persist_unpublish, run_after_change_hooks,
    },
};

type Result<T> = std::result::Result<T, ServiceError>;

/// Unpublish a versioned document on an existing connection/transaction.
///
/// Runs the full lifecycle: access check -> before-hooks -> set draft status ->
/// after-hooks -> hydrate -> strip read-denied fields.
/// Does NOT manage transactions — caller must open/commit.
pub fn unpublish_document_core(ctx: &ServiceContext, id: &str) -> Result<Document> {
    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();
    let write_hooks = ctx.write_hooks()?;
    let def = ctx.collection_def();

    let access =
        write_hooks.check_access(def.access.update.as_deref(), ctx.user, Some(id), None)?;

    if matches!(access, AccessResult::Denied) {
        return Err(ServiceError::AccessDenied("Update access denied".into()));
    }

    // When the hook returned Constrained filters, enforce row-level match.
    helpers::enforce_access_constraints(ctx, id, &access, "Update", false)?;

    let doc = query::find_by_id_raw(conn, ctx.slug, def, id, None, false)?.ok_or_else(|| {
        ServiceError::NotFound(format!("Document '{id}' not found in '{}'", ctx.slug))
    })?;

    let hook_ctx = HookContext::builder(ctx.slug, "update")
        .data(doc.fields.clone())
        .draft(true)
        .locale(None::<String>)
        .user(ctx.user)
        .build();

    let final_ctx =
        write_hooks.run_hooks_with_conn(&def.hooks, HookEvent::BeforeChange, hook_ctx, conn)?;

    persist_unpublish(ctx, id)?;

    let mut doc = doc;
    doc.fields
        .insert("_status".to_string(), Value::String("draft".into()));

    run_after_change_hooks(
        write_hooks,
        &def.hooks,
        &def.fields,
        &doc,
        AfterChangeInput::builder(ctx.slug, "update")
            .req_context(final_ctx.context)
            .user(ctx.user)
            .build(),
        conn,
    )?;

    query::hydrate_document(conn, ctx.slug, &def.fields, &mut doc, None, None)?;

    let mut read_denied = write_hooks.field_read_denied(&def.fields, ctx.user);
    read_denied.extend(helpers::collect_api_hidden_field_names(&def.fields, ""));

    doc.strip_fields(&read_denied);

    Ok(doc)
}

/// Unpublish a versioned document within a single transaction.
#[cfg(not(tarpaulin_include))]
pub fn unpublish_document(ctx: &ServiceContext, id: &str) -> Result<Document> {
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

    let doc = unpublish_document_core(&inner_ctx, id)?;

    tx.commit().context("Commit transaction")?;

    Ok(doc)
}
