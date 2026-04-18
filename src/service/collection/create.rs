//! Collection document creation.

use anyhow::Context as _;

use std::{cell::RefCell, rc::Rc};

use crate::{
    core::event::EventOperation,
    hooks::LuaCrudInfra,
    service::{
        RunnerWriteHooks, ServiceContext, ServiceError, WriteInput, WriteResult,
        create_document_core, flush_queue, flush_verification_queue,
    },
};

type Result<T> = std::result::Result<T, ServiceError>;

/// Create a document: before-hooks -> insert -> after-hooks.
///
/// **Pool mode** (`ctx.pool` set): opens a transaction, commits after success,
/// publishes events and clears cache after commit.
///
/// **Conn mode** (`ctx.conn` set, Lua CRUD path): runs on the existing
/// connection. Events are queued via `ctx.event_queue` for the parent to flush
/// after commit.
#[cfg(not(tarpaulin_include))]
pub fn create_document(ctx: &ServiceContext, input: WriteInput<'_>) -> Result<WriteResult> {
    if ctx.pool.is_some() {
        create_document_pool(ctx, input)
    } else {
        create_document_conn(ctx, input)
    }
}

/// Pool-based create: own transaction with event publishing after commit.
fn create_document_pool(ctx: &ServiceContext, input: WriteInput<'_>) -> Result<WriteResult> {
    let pool = ctx.pool.context("pool required")?;
    let runner = ctx.runner()?;
    let mut conn = pool.get().context("DB connection")?;
    let tx = conn.transaction_immediate().context("Start transaction")?;

    let queue = Rc::new(RefCell::new(Vec::new()));
    let vqueue = Rc::new(RefCell::new(Vec::new()));

    let infra = LuaCrudInfra::from_ctx(ctx, Some(queue.clone()), Some(vqueue.clone()));

    let mut wh = RunnerWriteHooks::new(runner)
        .with_conn(&tx)
        .with_infra(infra);

    if ctx.override_access {
        wh = wh.with_override_access();
    }

    let inner_ctx = ServiceContext::collection(ctx.slug, ctx.collection_def())
        .conn(&tx)
        .write_hooks(&wh)
        .user(ctx.user)
        .override_access(ctx.override_access)
        .cache(ctx.cache.clone())
        .event_transport(ctx.event_transport.clone())
        .event_queue(queue.clone())
        .verification_queue(vqueue.clone())
        .email_ctx(ctx.email_ctx.clone())
        .build();

    let result = create_document_core(&inner_ctx, input)?;
    drop(inner_ctx);

    tx.commit().context("Commit transaction")?;

    ctx.clear_cache();

    ctx.publish_mutation_event(EventOperation::Create, &result.0.id, &result.0.fields);
    flush_queue(ctx, &queue);
    ctx.maybe_send_verification(&result.0);
    flush_verification_queue(ctx, &vqueue);

    Ok(result)
}

/// Conn-based create: uses existing connection (Lua CRUD path).
fn create_document_conn(ctx: &ServiceContext, input: WriteInput<'_>) -> Result<WriteResult> {
    let result = create_document_core(ctx, input)?;

    ctx.clear_cache();

    ctx.publish_mutation_event(EventOperation::Create, &result.0.id, &result.0.fields);
    ctx.maybe_send_verification(&result.0);

    Ok(result)
}
