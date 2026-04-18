//! Collection document update.

use std::{cell::RefCell, rc::Rc};

use anyhow::Context as _;

use crate::{
    core::event::EventOperation,
    service::{
        RunnerWriteHooks, ServiceContext, ServiceError, WriteInput, WriteResult, flush_queue,
        update_document_core,
    },
};

type Result<T> = std::result::Result<T, ServiceError>;

/// Update a document within a single transaction: before-hooks -> update -> after-hooks -> commit.
/// When `draft` is true and the collection has drafts enabled, the update creates a version-only
/// save: the main table is NOT modified, only a new version snapshot is recorded.
// Excluded from coverage: requires HookRunner (Lua VM) for before/after hooks.
// Tested indirectly through CLI integration tests and gRPC API tests.
#[cfg(not(tarpaulin_include))]
pub fn update_document(
    ctx: &ServiceContext,
    id: &str,
    input: WriteInput<'_>,
) -> Result<WriteResult> {
    let pool = ctx.pool.context("pool required")?;
    let runner = ctx.runner()?;
    let mut conn = pool.get().context("DB connection")?;
    let tx = conn.transaction_immediate().context("Start transaction")?;

    let mut wh = RunnerWriteHooks::new(runner).with_conn(&tx);

    if ctx.override_access {
        wh = wh.with_override_access();
    }

    let queue = Rc::new(RefCell::new(Vec::new()));

    let inner_ctx = ServiceContext::collection(ctx.slug, ctx.collection_def())
        .conn(&tx)
        .write_hooks(&wh)
        .user(ctx.user)
        .override_access(ctx.override_access)
        .event_transport(ctx.event_transport.clone())
        .event_queue(queue.clone())
        .build();

    let result = update_document_core(&inner_ctx, id, input)?;
    drop(inner_ctx);

    tx.commit().context("Commit transaction")?;

    ctx.publish_mutation_event(
        EventOperation::Update,
        &result.0.id,
        result.0.fields.clone(),
    );
    flush_queue(ctx, &queue);

    Ok(result)
}
