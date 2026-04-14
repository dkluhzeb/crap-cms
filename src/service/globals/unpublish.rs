//! Global document unpublish.

use anyhow::Context as _;

use serde_json::Value;

use crate::{
    core::Document,
    db::{AccessResult, query, query::helpers::global_table},
    hooks::{HookContext, HookEvent},
    service::{
        AfterChangeInput, RunnerWriteHooks, ServiceContext, ServiceError, helpers,
        hooks::WriteHooks, run_after_change_hooks, unpublish_with_snapshot,
    },
};

type Result<T> = std::result::Result<T, ServiceError>;

/// Unpublish a global document within a single transaction.
#[cfg(not(tarpaulin_include))]
pub fn unpublish_global_document(ctx: &ServiceContext) -> Result<Document> {
    let pool = ctx.pool.context("pool required")?;
    let runner = ctx.runner()?;
    let def = ctx.global_def();
    let mut conn = pool.get().context("DB connection")?;
    let tx = conn.transaction_immediate().context("Start transaction")?;

    let mut wh = RunnerWriteHooks::new(runner).with_conn(&tx);

    if ctx.override_access {
        wh = wh.with_override_access();
    }

    // Access check
    let access = wh.check_access(def.access.update.as_deref(), ctx.user, None, None)?;

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

    let doc = query::get_global(&tx, ctx.slug, def, None)?;

    let hook_ctx = HookContext::builder(ctx.slug, "update")
        .data(doc.fields.clone())
        .draft(true)
        .locale(None::<String>)
        .user(ctx.user)
        .build();

    let final_ctx =
        runner.run_hooks_with_conn(&def.hooks, HookEvent::BeforeChange, hook_ctx, &tx)?;

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

    run_after_change_hooks(
        &wh,
        &def.hooks,
        &def.fields,
        &doc,
        AfterChangeInput::builder(ctx.slug, "update")
            .req_context(final_ctx.context)
            .user(ctx.user)
            .build(),
        &tx,
    )?;

    query::hydrate_document(&tx, &gtable, &def.fields, &mut doc, None, None)?;

    let mut read_denied = wh.field_read_denied(&def.fields, ctx.user);
    read_denied.extend(helpers::collect_hidden_field_names(&def.fields, ""));

    doc.strip_fields(&read_denied);

    tx.commit().context("Commit transaction")?;

    Ok(doc)
}
