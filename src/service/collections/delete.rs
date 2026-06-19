//! Collection document deletion.

use std::{cell::RefCell, rc::Rc};

use anyhow::Context as _;

use crate::{
    config::LocaleConfig,
    core::{
        ReqContext,
        upload::{self, StorageBackend},
    },
    hooks::LuaCrudInfra,
    service::{
        RunnerWriteHooks, ServiceContext, ServiceError, delete_document_in_conn, flush_queue,
        invalidate_user_streams_if_auth,
    },
};

type Result<T> = std::result::Result<T, ServiceError>;

/// Delete a document: before-hooks -> delete -> after-hooks.
///
/// **Pool mode** (`ctx.pool` set): opens a transaction, commits after success.
/// **Conn mode** (`ctx.conn` set, Lua CRUD path): runs on the existing connection.
///
/// # Errors
///
/// Returns service-layer errors (access denied, ref-count protection, hook
/// errors) or a backend error if the DB transaction or persistence fails.
#[cfg(not(tarpaulin_include))]
pub fn delete_document(
    ctx: &ServiceContext,
    id: &str,
    storage: Option<&dyn StorageBackend>,
    locale_config: Option<&LocaleConfig>,
) -> Result<ReqContext> {
    if ctx.pool.is_some() {
        delete_document_pool(ctx, id, storage, locale_config)
    } else {
        delete_document_conn(ctx, id, storage, locale_config)
    }
}

/// Pool-based delete: own transaction with event publishing after commit.
fn delete_document_pool(
    ctx: &ServiceContext,
    id: &str,
    storage: Option<&dyn StorageBackend>,
    locale_config: Option<&LocaleConfig>,
) -> Result<ReqContext> {
    let pool = ctx.pool.context("pool required")?;
    let runner = ctx.runner()?;
    let def = ctx.collection_def()?;
    let mut conn = pool.get().context("DB connection")?;
    let tx = conn.transaction_immediate().context("Start transaction")?;

    let queue = Rc::new(RefCell::new(Vec::new()));

    let infra = LuaCrudInfra::from_ctx(ctx, Some(queue.clone()), None);

    let mut wh = RunnerWriteHooks::new(runner)
        .with_conn(&tx)
        .with_infra(infra);

    if ctx.override_access {
        wh = wh.with_override_access();
    }

    let inner_ctx = ServiceContext::collection(ctx.slug, def)
        .conn(&tx)
        .write_hooks(&wh)
        .user(ctx.user)
        .override_access(ctx.override_access)
        .cache(ctx.cache.clone())
        .event_transport(ctx.event_transport.clone())
        .event_queue(queue.clone())
        .build();

    let result = delete_document_in_conn(&inner_ctx, id, locale_config)?;
    drop(inner_ctx);

    tx.commit().context("Commit transaction")?;

    ctx.clear_cache();

    ctx.publish_delete_event(id, def.soft_delete, result.pre_status.clone());
    // Deleting an auth document revokes that user — tear down their live streams
    // post-commit. This applies to BOTH hard and soft delete: the per-request
    // evaluator resolves users via `find_by_id`, which excludes soft-deleted
    // rows, so a trashed user is already rejected (`UserMissing`) on new
    // requests; their open SSE/subscribe streams (which never re-resolve) must be
    // torn down too. No-op for non-auth collections.
    invalidate_user_streams_if_auth(ctx, id);
    flush_queue(ctx, &queue);

    // Clean up upload files after successful commit (skip for soft-delete to allow restore)
    if !def.soft_delete
        && let (Some(s), Some(fields)) = (storage, result.upload_doc_fields)
    {
        upload::delete_upload_files(s, &fields);
    }

    Ok(result.context)
}

/// Conn-based delete: uses existing connection (Lua CRUD path).
fn delete_document_conn(
    ctx: &ServiceContext,
    id: &str,
    storage: Option<&dyn StorageBackend>,
    locale_config: Option<&LocaleConfig>,
) -> Result<ReqContext> {
    let def = ctx.collection_def()?;
    let result = delete_document_in_conn(ctx, id, locale_config)?;

    ctx.clear_cache();

    ctx.publish_delete_event(id, def.soft_delete, result.pre_status.clone());
    // Deleting an auth document revokes that user — tear down their live streams
    // (conn mode fires immediate). Applies to both hard and soft delete: a
    // soft-deleted user is rejected by the evaluator's `find_by_id` on new
    // requests, so their open streams must be closed too. See the pool path.
    invalidate_user_streams_if_auth(ctx, id);

    if !def.soft_delete
        && let (Some(s), Some(fields)) = (storage, result.upload_doc_fields)
    {
        upload::delete_upload_files(s, &fields);
    }

    Ok(result.context)
}
