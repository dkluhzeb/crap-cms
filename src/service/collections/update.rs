//! Collection document update.

use std::{cell::RefCell, rc::Rc};

use anyhow::Context as _;

use crate::{
    core::event::EventOperation,
    hooks::LuaCrudInfra,
    service::{
        RunnerWriteHooks, ServiceContext, ServiceError, WriteInput, WriteResult, flush_queue,
        flush_verification_queue, invalidate_user_streams_if_auth, update_document_in_conn,
    },
};

type Result<T> = std::result::Result<T, ServiceError>;

/// Update a document: before-hooks -> update -> after-hooks.
///
/// **Pool mode** (`ctx.pool` set): opens a transaction, commits after success.
/// **Conn mode** (`ctx.conn` set, Lua CRUD path): runs on the existing connection.
///
/// # Errors
///
/// Returns service-layer errors (access denied, validation, hook errors) or
/// a backend error if the DB transaction or persistence fails.
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
    let mut conn = pool.write().context("DB connection")?;
    let tx = conn.transaction_immediate().context("Start transaction")?;

    let queue = Rc::new(RefCell::new(Vec::new()));

    // The verification queue exists for NESTED Lua creates: a hook running
    // inside this transaction that creates a verify-email auth document must
    // get its verification mail sent after commit — without the queue it was
    // silently dropped (only the create orchestrators used to carry one).
    let vqueue = Rc::new(RefCell::new(Vec::new()));

    let infra = LuaCrudInfra::from_ctx(ctx, Some(queue.clone()), Some(vqueue.clone()));

    let mut wh = RunnerWriteHooks::new(runner)
        .with_conn(&tx)
        .with_infra(infra);

    if ctx.override_access {
        wh = wh.with_override_access();
    }

    let inner_ctx = ServiceContext::collection(ctx.slug, ctx.collection_def()?)
        .conn(&tx)
        .write_hooks(&wh)
        .inherit_write_infra(ctx)
        .event_queue(queue.clone())
        .build();

    let result = update_document_in_conn(&inner_ctx, id, input)?;
    drop(inner_ctx);

    tx.commit().context("Commit transaction")?;

    ctx.clear_cache();

    ctx.publish_mutation_event(EventOperation::Update, &result.0.id, &result.0.fields);
    invalidate_user_streams_if_auth(ctx, &result.0.id);
    flush_queue(ctx, &queue);
    flush_verification_queue(ctx, &vqueue);

    Ok(result)
}

/// Conn-based update: uses existing connection (Lua CRUD path).
fn update_document_conn(
    ctx: &ServiceContext,
    id: &str,
    input: WriteInput<'_>,
) -> Result<WriteResult> {
    let result = update_document_in_conn(ctx, id, input)?;

    ctx.clear_cache();

    ctx.publish_mutation_event(EventOperation::Update, &result.0.id, &result.0.fields);
    invalidate_user_streams_if_auth(ctx, &result.0.id);

    Ok(result)
}
