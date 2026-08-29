//! Collection document creation.

use crate::{
    core::event::EventOperation,
    service::{
        ServiceContext, ServiceError, WriteInput, WriteResult, create_document_in_conn,
        run_pool_write,
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
///
/// # Errors
///
/// Returns service-layer errors (access denied, validation, hook errors) or
/// a backend error if the DB transaction or persistence fails.
#[cfg(not(tarpaulin_include))]
pub fn create_document(ctx: &ServiceContext, input: WriteInput<'_>) -> Result<WriteResult> {
    if ctx.pool.is_some() {
        create_document_pool(ctx, input)
    } else {
        create_document_conn(ctx, input)
    }
}

/// Pool-based create: the shared write envelope with create's post-commit
/// effects (mutation event + verification email for auth collections).
fn create_document_pool(ctx: &ServiceContext, input: WriteInput<'_>) -> Result<WriteResult> {
    run_pool_write(
        ctx,
        None,
        |inner| create_document_in_conn(inner, input),
        |ctx, result| {
            ctx.publish_mutation_event(EventOperation::Create, &result.0.id, &result.0.fields);
            ctx.maybe_send_verification(&result.0);
        },
    )
}

/// Conn-based create: uses existing connection (Lua CRUD path).
fn create_document_conn(ctx: &ServiceContext, input: WriteInput<'_>) -> Result<WriteResult> {
    let result = create_document_in_conn(ctx, input)?;

    ctx.clear_cache();

    ctx.publish_mutation_event(EventOperation::Create, &result.0.id, &result.0.fields);
    ctx.maybe_send_verification(&result.0);

    Ok(result)
}
