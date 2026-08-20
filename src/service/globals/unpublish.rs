//! Global document unpublish.

use std::{cell::RefCell, rc::Rc};

use anyhow::Context as _;

use serde_json::Value;

use crate::{
    core::{Document, event::EventOperation},
    db::{AccessResult, LocaleContext, query, query::helpers::global_table},
    hooks::{AccessCheckInput, HookContext, HookEvent, LuaCrudInfra},
    service::{
        AfterChangeInput, RunnerWriteHooks, ServiceContext, ServiceError, flush_queue, helpers,
        run_after_change_hooks, unpublish_with_snapshot,
    },
};

type Result<T> = std::result::Result<T, ServiceError>;

/// Unpublish a global — sets `_status` to `"draft"` without modifying the
/// stored field data. Only meaningful on globals with `versions`.
///
/// **Pool mode** (`ctx.pool` set): opens a transaction, commits after success.
/// **Conn mode** (`ctx.conn` set, Lua CRUD path): runs on the existing
/// connection so an unpublish inside a hook joins the caller's transaction.
///
/// # Errors
///
/// Returns service-layer errors (access denied, hook errors) or a backend
/// error if the DB transaction or persistence fails.
#[cfg(not(tarpaulin_include))]
pub fn unpublish_global_document(ctx: &ServiceContext) -> Result<Document> {
    if ctx.pool.is_some() {
        unpublish_global_pool(ctx)
    } else {
        unpublish_global_conn(ctx)
    }
}

/// Conn-mode core: everything except transaction/commit and post-commit
/// event/cache side effects. Shared by both dispatch modes.
fn unpublish_global_in_conn(ctx: &ServiceContext) -> Result<Document> {
    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();
    let write_hooks = ctx.write_hooks()?;
    let def = ctx.global_def()?;

    let access = write_hooks.check_access(
        &AccessCheckInput::builder("unpublish", ctx.slug)
            .access(def.access.update.as_ref())
            .user(ctx.user)
            .build(),
    )?;

    if matches!(access, AccessResult::Denied) {
        return Err(ServiceError::AccessDenied("Update access denied".into()));
    }

    if matches!(access, AccessResult::Constrained(_)) {
        return Err(ServiceError::HookError(format!(
            "Access hook for global '{}' returned a filter table; globals don't support filter-based access — return true/false based on ctx.user fields instead.",
            ctx.slug
        )));
    }

    let gtable = global_table(ctx.slug);

    // Same locale-aware read fix as the collection unpublish path: when the
    // global has localized fields and locales are enabled, the fallback in
    // `get_global` emits bare column names (`title`) instead of locale-
    // suffixed ones (`title__en`), failing with `no such column`. Build a
    // default LocaleContext from the attached config to fetch all locales.
    let locale_ctx = ctx.default_locale_ctx();

    let doc = query::get_global(conn, ctx.slug, def, locale_ctx.as_ref())?;

    let hook_ctx = HookContext::builder(ctx.slug, "update")
        .data(doc.fields.clone())
        .document_id("default")
        .draft(true)
        .locale(locale_ctx.as_ref().map(LocaleContext::access_locale))
        .user(ctx.user)
        .build();

    // Through the WriteHooks adapter (NOT the raw runner): the adapter
    // carries the LuaCrudInfra, so nested CRUD inside the hook queues
    // mutation events and invalidates caches — and honors hooks_enabled.
    let final_ctx =
        write_hooks.run_hooks_with_conn(&def.hooks, HookEvent::BeforeChange, hook_ctx, conn)?;

    unpublish_with_snapshot(
        conn,
        &gtable,
        "default",
        &def.fields,
        def.versions.as_ref(),
        &doc,
    )?;

    let mut doc = doc;
    doc.fields
        .insert("_status".to_string(), Value::String("draft".into()));

    // Hydrate join fields BEFORE after-change hooks so they see nested data.
    query::hydrate_document(conn, &gtable, &def.fields, &mut doc, None, None)?;

    run_after_change_hooks(
        write_hooks,
        &def.hooks,
        &def.fields,
        &doc,
        AfterChangeInput::builder(ctx.slug, "update")
            .draft(true)
            .locale(locale_ctx.as_ref().map(|lc| lc.access_locale().to_string()))
            .req_context(final_ctx.context)
            .user(ctx.user)
            .build(),
        conn,
    )?;

    write_hooks.strip_read_access_doc(&def.fields, &mut doc, ctx.slug, ctx.user, None);
    doc.strip_fields(&helpers::collect_api_hidden_field_names(&def.fields, ""));

    Ok(doc)
}

/// Conn mode (Lua CRUD): the caller owns the transaction and the event-queue
/// flush. Queue the mutation event and invalidate cache; the outer tx-commit
/// flushes the queue.
fn unpublish_global_conn(ctx: &ServiceContext) -> Result<Document> {
    let doc = unpublish_global_in_conn(ctx)?;

    ctx.clear_cache();
    ctx.publish_mutation_event(EventOperation::Update, &doc.id, &doc.fields);

    Ok(doc)
}

/// Pool mode: open a transaction, run the core, commit, then run the
/// post-commit side effects.
#[cfg(not(tarpaulin_include))]
fn unpublish_global_pool(ctx: &ServiceContext) -> Result<Document> {
    let pool = ctx.pool.context("pool required")?;
    let runner = ctx.runner()?;
    let def = ctx.global_def()?;
    let mut conn = pool.get().context("DB connection")?;
    let tx = conn.transaction_immediate().context("Start transaction")?;

    let queue = Rc::new(RefCell::new(Vec::new()));

    let infra = LuaCrudInfra::from_ctx(ctx, Some(queue.clone()), None);

    let mut wh = RunnerWriteHooks::new(runner)
        .with_conn(&tx)
        .with_infra(infra);

    if ctx.override_access {
        wh = wh.with_override_access();
    }

    let inner_ctx = ServiceContext::global(ctx.slug, def)
        .conn(&tx)
        .write_hooks(&wh)
        .inherit_write_infra(ctx)
        .event_queue(queue.clone())
        .locale_config(ctx.locale_config)
        .build();

    let doc = unpublish_global_in_conn(&inner_ctx)?;
    drop(inner_ctx);

    tx.commit().context("Commit transaction")?;

    ctx.clear_cache();

    // Same post-commit sequence as `update_global_document` / the collection
    // unpublish path: notify subscribers of the status change, then flush any
    // events queued by nested hook CRUD.
    ctx.publish_mutation_event(EventOperation::Update, &doc.id, &doc.fields);
    flush_queue(ctx, &queue);

    Ok(doc)
}
