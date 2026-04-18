//! Collection document deletion.

use std::{cell::RefCell, collections::HashMap, rc::Rc};

use anyhow::Context as _;
use serde_json::Value;

use crate::{
    config::LocaleConfig,
    core::{
        event::EventOperation,
        upload::{self, StorageBackend},
    },
    hooks::LuaCrudInfra,
    service::{RunnerWriteHooks, ServiceContext, ServiceError, delete_document_core, flush_queue},
};

type Result<T> = std::result::Result<T, ServiceError>;

/// Delete a document: before-hooks -> delete -> after-hooks.
///
/// **Pool mode** (`ctx.pool` set): opens a transaction, commits after success.
/// **Conn mode** (`ctx.conn` set, Lua CRUD path): runs on the existing connection.
#[cfg(not(tarpaulin_include))]
pub fn delete_document(
    ctx: &ServiceContext,
    id: &str,
    storage: Option<&dyn StorageBackend>,
    locale_config: Option<&LocaleConfig>,
) -> Result<HashMap<String, Value>> {
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
) -> Result<HashMap<String, Value>> {
    let pool = ctx.pool.context("pool required")?;
    let runner = ctx.runner()?;
    let def = ctx.collection_def();
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
        .invalidation_transport(ctx.invalidation_transport.clone())
        .event_transport(ctx.event_transport.clone())
        .event_queue(queue.clone())
        .build();

    let result = delete_document_core(&inner_ctx, id, locale_config)?;
    drop(inner_ctx);

    tx.commit().context("Commit transaction")?;

    ctx.clear_cache();

    ctx.publish_mutation_event(EventOperation::Delete, id, &HashMap::new());
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
) -> Result<HashMap<String, Value>> {
    let def = ctx.collection_def();
    let result = delete_document_core(ctx, id, locale_config)?;

    ctx.clear_cache();

    ctx.publish_mutation_event(EventOperation::Delete, id, &HashMap::new());

    if !def.soft_delete
        && let (Some(s), Some(fields)) = (storage, result.upload_doc_fields)
    {
        upload::delete_upload_files(s, &fields);
    }

    Ok(result.context)
}
