//! Collection document deletion.

use std::collections::HashMap;

use anyhow::Context as _;
use serde_json::Value;

use crate::{
    config::LocaleConfig,
    core::upload::{self, StorageBackend},
    service::{RunnerWriteHooks, ServiceContext, ServiceError, delete_document_core},
};

type Result<T> = std::result::Result<T, ServiceError>;

/// Delete a document within a single transaction: before-hooks -> delete -> after-hooks -> commit.
/// If `storage` is provided and the collection is an upload collection,
/// upload files are cleaned up after successful deletion.
// Excluded from coverage: requires HookRunner (Lua VM) for before/after hooks.
// Tested indirectly through CLI integration tests and gRPC API tests.
#[cfg(not(tarpaulin_include))]
pub fn delete_document(
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

    let mut wh = RunnerWriteHooks::new(runner).with_conn(&tx);

    if ctx.override_access {
        wh = wh.with_override_access();
    }

    let inner_ctx = ServiceContext::collection(ctx.slug, def)
        .conn(&tx)
        .write_hooks(&wh)
        .user(ctx.user)
        .override_access(ctx.override_access)
        .invalidation_transport(ctx.invalidation_transport.clone())
        .build();

    let result = delete_document_core(&inner_ctx, id, locale_config)?;

    tx.commit().context("Commit transaction")?;

    // Clean up upload files after successful commit (skip for soft-delete to allow restore)
    if !def.soft_delete
        && let (Some(s), Some(fields)) = (storage, result.upload_doc_fields)
    {
        upload::delete_upload_files(s, &fields);
    }

    Ok(result.context)
}
