//! Global document unpublish.

use anyhow::Context as _;

use crate::{
    core::{Document, collection::GlobalDefinition},
    db::{DbPool, query::helpers::global_table},
    hooks::{HookContext, HookEvent, HookRunner},
    service::{AfterChangeInput, RunnerWriteHooks, ServiceError, run_after_change_hooks},
};

type Result<T> = std::result::Result<T, ServiceError>;

/// Unpublish a global document within a single transaction: before-hooks -> unpublish -> after-hooks.
#[cfg(not(tarpaulin_include))]
pub fn unpublish_global_document(
    pool: &DbPool,
    runner: &HookRunner,
    slug: &str,
    def: &GlobalDefinition,
    user: Option<&Document>,
) -> Result<Document> {
    let mut conn = pool.get().context("DB connection")?;
    let tx = conn.transaction_immediate().context("Start transaction")?;

    let global_table = global_table(slug);
    let doc = crate::db::query::get_global(&tx, slug, def, None)?;

    let hook_ctx = HookContext::builder(slug, "update")
        .data(doc.fields.clone())
        .draft(true)
        .locale(None::<String>)
        .user(user)
        .build();
    let final_ctx =
        runner.run_hooks_with_conn(&def.hooks, HookEvent::BeforeChange, hook_ctx, &tx)?;

    crate::service::unpublish_with_snapshot(
        &tx,
        &global_table,
        "default",
        &def.fields,
        def.versions.as_ref(),
        &doc,
    )?;

    let wh = RunnerWriteHooks::new(runner);
    run_after_change_hooks(
        &wh,
        &def.hooks,
        &def.fields,
        &doc,
        AfterChangeInput::builder(slug, "update")
            .req_context(final_ctx.context)
            .user(user)
            .build(),
        &tx,
    )?;

    tx.commit().context("Commit transaction")?;
    Ok(doc)
}
