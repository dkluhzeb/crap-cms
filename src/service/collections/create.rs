//! Collection document creation.

use crate::{
    core::event::EventOperation,
    service::{
        ServiceContext, ServiceError, WriteInput, WriteResult, create_document_gated,
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
    let (result, _) = run_pool_write(
        ctx,
        None,
        |inner| {
            let written = create_document_gated(inner, input)?;

            // Inside the transaction, not after it: the account, its
            // verification token, and the queued email commit together or
            // not at all. Minting the token post-commit leaves a window in
            // which a stop or crash yields an account nobody can verify and
            // nothing queued to retry.
            inner.maybe_send_verification(&written.0.0)?;

            Ok(written)
        },
        |ctx, (result, row)| {
            ctx.publish_mutation_event(EventOperation::Create, &result.0.id, row.clone());
        },
    )?;

    Ok(result)
}

/// Conn-based create: uses existing connection (Lua CRUD path).
fn create_document_conn(ctx: &ServiceContext, input: WriteInput<'_>) -> Result<WriteResult> {
    let (result, row) = create_document_gated(ctx, input)?;

    ctx.clear_cache();

    ctx.publish_mutation_event(EventOperation::Create, &result.0.id, row);
    ctx.maybe_send_verification(&result.0)?;

    Ok(result)
}
