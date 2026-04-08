//! Collection document unpublish.

use anyhow::Context as _;

use crate::{
    core::{CollectionDefinition, Document},
    db::{DbPool, query},
    hooks::{HookContext, HookEvent, HookRunner},
    service::{AfterChangeInput, RunnerWriteHooks, ServiceError, run_after_change_hooks},
};

type Result<T> = std::result::Result<T, ServiceError>;

/// Unpublish a versioned document: before-hooks -> set draft status -> after-hooks -> commit.
// Excluded from coverage: requires HookRunner (Lua VM) for before/after hooks.
// Tested indirectly through CLI integration tests and gRPC API tests.
#[cfg(not(tarpaulin_include))]
pub fn unpublish_document(
    pool: &DbPool,
    runner: &HookRunner,
    slug: &str,
    id: &str,
    def: &CollectionDefinition,
    user: Option<&Document>,
) -> Result<Document> {
    let mut conn = pool.get().context("DB connection")?;
    let tx = conn.transaction_immediate().context("Start transaction")?;

    let doc = query::find_by_id_raw(&tx, slug, def, id, None, false)?
        .ok_or_else(|| ServiceError::NotFound(format!("Document '{id}' not found in '{slug}'")))?;

    let hook_ctx = HookContext::builder(slug, "update")
        .data(doc.fields.clone())
        .draft(true)
        .locale(None::<String>)
        .user(user)
        .build();
    let final_ctx =
        runner.run_hooks_with_conn(&def.hooks, HookEvent::BeforeChange, hook_ctx, &tx)?;

    crate::service::persist_unpublish(&tx, slug, id, def)?;

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
