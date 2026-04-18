//! Collection document update.

use std::{cell::RefCell, rc::Rc};

use anyhow::Context as _;

use crate::{
    core::event::EventOperation,
    hooks::LuaCrudInfra,
    service::{
        RunnerWriteHooks, ServiceContext, ServiceError, WriteInput, WriteResult, flush_queue,
        update_document_core,
    },
};

type Result<T> = std::result::Result<T, ServiceError>;

/// Update a document: before-hooks -> update -> after-hooks.
///
/// **Pool mode** (`ctx.pool` set): opens a transaction, commits after success.
/// **Conn mode** (`ctx.conn` set, Lua CRUD path): runs on the existing connection.
#[cfg(not(tarpaulin_include))]
pub fn update_document(
    ctx: &ServiceContext,
    id: &str,
    input: WriteInput<'_>,
) -> Result<WriteResult> {
    if ctx.pool.is_some() {
        update_document_pool(ctx, id, input)
    } else {
        update_document_conn(ctx, id, input)
    }
}

/// Pool-based update: own transaction with event publishing after commit.
fn update_document_pool(
    ctx: &ServiceContext,
    id: &str,
    input: WriteInput<'_>,
) -> Result<WriteResult> {
    let pool = ctx.pool.context("pool required")?;
    let runner = ctx.runner()?;
    let mut conn = pool.get().context("DB connection")?;
    let tx = conn.transaction_immediate().context("Start transaction")?;

    let queue = Rc::new(RefCell::new(Vec::new()));

    let infra = LuaCrudInfra {
        event_transport: ctx.event_transport.clone(),
        cache: ctx.cache.clone(),
        event_queue: Some(queue.clone()),
        verification_queue: None,
    };

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
        .build();

    let result = update_document_core(&inner_ctx, id, input)?;
    drop(inner_ctx);

    tx.commit().context("Commit transaction")?;

    ctx.clear_cache();

    ctx.publish_mutation_event(
        EventOperation::Update,
        &result.0.id,
        result.0.fields.clone(),
    );
    flush_queue(ctx, &queue);

    Ok(result)
}

/// Conn-based update: uses existing connection (Lua CRUD path).
fn update_document_conn(
    ctx: &ServiceContext,
    id: &str,
    input: WriteInput<'_>,
) -> Result<WriteResult> {
    let result = update_document_core(ctx, id, input)?;

    ctx.clear_cache();

    ctx.publish_mutation_event(
        EventOperation::Update,
        &result.0.id,
        result.0.fields.clone(),
    );

    Ok(result)
}
