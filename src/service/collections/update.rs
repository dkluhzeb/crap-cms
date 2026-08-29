//! Collection document update.

use crate::{
    core::event::EventOperation,
    service::{
        ServiceContext, ServiceError, WriteInput, WriteResult, invalidate_user_streams_if_auth,
        run_pool_write, update_document_in_conn,
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
    run_pool_write(
        ctx,
        None,
        |inner| update_document_in_conn(inner, id, input),
        |ctx, result| {
            ctx.publish_mutation_event(EventOperation::Update, &result.0.id, &result.0.fields);
            // Editing an auth document can change a user's access — tear down
            // their live streams post-commit.
            invalidate_user_streams_if_auth(ctx, &result.0.id);
        },
    )
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
