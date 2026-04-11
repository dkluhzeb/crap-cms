//! Collection document creation.

use anyhow::Context as _;

use crate::service::{
    RunnerWriteHooks, ServiceContext, ServiceError, WriteInput, WriteResult, create_document_core,
};

type Result<T> = std::result::Result<T, ServiceError>;

/// Create a document within a single transaction: before-hooks -> insert -> after-hooks -> commit.
/// When `draft` is true and the collection has drafts enabled, the document is created with
/// `_status = 'draft'` and required-field validation is skipped.
// Excluded from coverage: requires HookRunner (Lua VM) for before/after hooks.
// Tested indirectly through CLI integration tests and gRPC API tests.
#[cfg(not(tarpaulin_include))]
pub fn create_document(ctx: &ServiceContext, input: WriteInput<'_>) -> Result<WriteResult> {
    let pool = ctx.pool.context("pool required")?;
    let runner = ctx.runner()?;
    let mut conn = pool.get().context("DB connection")?;
    let tx = conn.transaction_immediate().context("Start transaction")?;

    let mut wh = RunnerWriteHooks::new(runner).with_conn(&tx);

    if ctx.override_access {
        wh = wh.with_override_access();
    }

    let inner_ctx = ServiceContext::collection(ctx.slug, ctx.collection_def())
        .conn(&tx)
        .write_hooks(&wh)
        .user(ctx.user)
        .override_access(ctx.override_access)
        .build();

    let result = create_document_core(&inner_ctx, input)?;

    tx.commit().context("Commit transaction")?;

    Ok(result)
}
