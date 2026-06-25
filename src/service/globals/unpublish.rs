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
        hooks::WriteHooks, run_after_change_hooks, unpublish_with_snapshot,
    },
};

type Result<T> = std::result::Result<T, ServiceError>;

/// Unpublish a global document within a single transaction.
///
/// # Errors
///
/// Returns service-layer errors (access denied, hook errors) or a backend
/// error if the DB transaction or persistence fails.
#[cfg(not(tarpaulin_include))]
pub fn unpublish_global_document(ctx: &ServiceContext) -> Result<Document> {
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

    // Access check
    let access = wh.check_access(
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

    let doc = query::get_global(&tx, ctx.slug, def, locale_ctx.as_ref())?;

    let hook_ctx = HookContext::builder(ctx.slug, "update")
        .data(doc.fields.clone())
        .document_id("default")
        .draft(true)
        .locale(locale_ctx.as_ref().map(LocaleContext::access_locale))
        .user(ctx.user)
        .build();

    let final_ctx =
        runner.run_hooks_with_conn(&def.hooks, HookEvent::BeforeChange, hook_ctx, &tx, None)?;

    unpublish_with_snapshot(
        &tx,
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
    query::hydrate_document(&tx, &gtable, &def.fields, &mut doc, None, None)?;

    run_after_change_hooks(
        &wh,
        &def.hooks,
        &def.fields,
        &doc,
        AfterChangeInput::builder(ctx.slug, "update")
            .draft(true)
            .locale(locale_ctx.as_ref().map(|lc| lc.access_locale().to_string()))
            .req_context(final_ctx.context)
            .user(ctx.user)
            .build(),
        &tx,
    )?;

    wh.strip_read_access_doc(&def.fields, &mut doc, ctx.slug, ctx.user, None);
    doc.strip_fields(&helpers::collect_api_hidden_field_names(&def.fields, ""));

    tx.commit().context("Commit transaction")?;

    ctx.clear_cache();

    // Same post-commit sequence as `update_global_document` / the collection
    // unpublish path: notify subscribers of the status change, then flush any
    // events queued by nested hook CRUD.
    ctx.publish_mutation_event(EventOperation::Update, &doc.id, &doc.fields);
    flush_queue(ctx, &queue);

    Ok(doc)
}
