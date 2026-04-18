//! Bulk delete — delete multiple documents matching a filter.

use std::{cell::RefCell, collections::HashMap, rc::Rc};

use anyhow::Context as _;
use serde_json::Value;

use crate::{
    config::LocaleConfig,
    core::event::EventOperation,
    db::{FilterClause, FindQuery, query},
    service::{RunnerWriteHooks, ServiceContext, ServiceError, delete_document_core, flush_queue},
};

const BATCH_SIZE: i64 = 500;

type Result<T> = std::result::Result<T, ServiceError>;

/// Result of a bulk delete operation.
pub struct DeleteManyResult {
    pub hard_deleted: i64,
    pub soft_deleted: i64,
    pub skipped: i64,
    pub deleted_ids: Vec<String>,
    pub upload_fields_to_clean: Vec<HashMap<String, Value>>,
}

/// Options controlling bulk delete behavior.
pub struct DeleteManyOptions {
    /// Whether to run lifecycle hooks per document. Defaults to `true`.
    pub run_hooks: bool,
    /// Whether to include soft-deleted rows in the query. Required when
    /// emptying the trash (finding rows with `_deleted_at EXISTS`).
    pub include_deleted: bool,
}

impl Default for DeleteManyOptions {
    fn default() -> Self {
        Self {
            run_hooks: true,
            include_deleted: false,
        }
    }
}

/// Delete multiple documents matching the given filters.
///
/// **Pool mode** (`ctx.pool` set): processes in batches — find → delete → commit → repeat.
/// Each batch gets its own transaction and write hooks. Events are published after each
/// batch commit.
///
/// **Conn mode** (`ctx.conn` set, Lua path): finds all matching docs on the existing
/// connection and deletes them one by one.
pub fn delete_many(
    ctx: &ServiceContext,
    filters: Vec<FilterClause>,
    locale_config: &LocaleConfig,
    opts: &DeleteManyOptions,
) -> Result<DeleteManyResult> {
    if ctx.pool.is_some() {
        delete_many_pool(ctx, filters, locale_config, opts)
    } else {
        delete_many_conn(ctx, filters, locale_config, opts)
    }
}

/// Pool-based bulk delete: batched transactions with event publishing after each commit.
fn delete_many_pool(
    ctx: &ServiceContext,
    filters: Vec<FilterClause>,
    locale_config: &LocaleConfig,
    opts: &DeleteManyOptions,
) -> Result<DeleteManyResult> {
    let pool = ctx.pool.context("pool required")?;
    let runner = ctx.runner()?;
    let def = ctx.collection_def();

    let mut hard_count = 0i64;
    let mut soft_count = 0i64;
    let mut skipped_count = 0i64;
    let mut upload_fields_to_clean = Vec::new();
    let mut deleted_ids = Vec::new();

    loop {
        let mut conn = pool.get().context("DB connection")?;
        let tx = conn
            .transaction_immediate()
            .context("Start delete transaction")?;

        let batch_query = FindQuery::builder()
            .filters(filters.clone())
            .limit(BATCH_SIZE)
            .include_deleted(opts.include_deleted)
            .build();

        let docs =
            query::find(&tx, ctx.slug, def, &batch_query, None).context("Find batch for delete")?;

        if docs.is_empty() {
            tx.commit().context("Commit final transaction")?;
            break;
        }

        let mut wh = RunnerWriteHooks::new(runner)
            .with_hooks_enabled(opts.run_hooks)
            .with_conn(&tx);

        if ctx.override_access {
            wh = wh.with_override_access();
        }

        let queue = Rc::new(RefCell::new(Vec::new()));

        let inner_ctx = ServiceContext::collection(ctx.slug, def)
            .conn(&tx)
            .write_hooks(&wh)
            .user(ctx.user)
            .override_access(ctx.override_access)
            .invalidation_transport(ctx.invalidation_transport.clone())
            .event_transport(ctx.event_transport.clone())
            .cache(ctx.cache.clone())
            .event_queue(queue.clone())
            .build();

        let batch_len = docs.len();
        let mut batch_deleted = 0usize;

        for doc in &docs {
            match delete_document_core(&inner_ctx, &doc.id, Some(locale_config)) {
                Ok(result) => {
                    if def.soft_delete {
                        soft_count += 1;
                    } else {
                        hard_count += 1;
                        if let Some(fields) = result.upload_doc_fields {
                            upload_fields_to_clean.push(fields);
                        }
                    }
                    deleted_ids.push(doc.id.to_string());
                    batch_deleted += 1;
                }
                Err(ServiceError::Referenced { .. }) => {
                    skipped_count += 1;
                }
                Err(e) => return Err(e),
            }
        }

        drop(inner_ctx);

        tx.commit().context("Commit delete transaction")?;

        ctx.clear_cache();

        // Publish events for this batch after commit.
        for id in deleted_ids.iter().skip(deleted_ids.len() - batch_deleted) {
            ctx.publish_mutation_event(EventOperation::Delete, id, Default::default());
        }
        flush_queue(ctx, &queue);

        // If nothing was deleted in this batch, all remaining matches are
        // referenced — stop to avoid an infinite loop.
        if batch_deleted == 0 {
            skipped_count = batch_len as i64;
            break;
        }
    }

    Ok(DeleteManyResult {
        hard_deleted: hard_count,
        soft_deleted: soft_count,
        skipped: skipped_count,
        deleted_ids,
        upload_fields_to_clean,
    })
}

/// Conn-based bulk delete: uses existing connection (Lua CRUD path).
fn delete_many_conn(
    ctx: &ServiceContext,
    filters: Vec<FilterClause>,
    locale_config: &LocaleConfig,
    opts: &DeleteManyOptions,
) -> Result<DeleteManyResult> {
    let def = ctx.collection_def();

    let find_query = FindQuery::builder()
        .filters(filters)
        .include_deleted(opts.include_deleted)
        .build();

    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();

    let docs =
        query::find(conn, ctx.slug, def, &find_query, None).context("Find docs for delete")?;

    let mut hard_count = 0i64;
    let mut soft_count = 0i64;
    let mut skipped_count = 0i64;
    let mut upload_fields_to_clean = Vec::new();
    let mut deleted_ids = Vec::new();

    for doc in &docs {
        match delete_document_core(ctx, &doc.id, Some(locale_config)) {
            Ok(result) => {
                if def.soft_delete {
                    soft_count += 1;
                } else {
                    hard_count += 1;
                    if let Some(fields) = result.upload_doc_fields {
                        upload_fields_to_clean.push(fields);
                    }
                }
                deleted_ids.push(doc.id.to_string());
            }
            Err(ServiceError::Referenced { .. }) => {
                skipped_count += 1;
            }
            Err(e) => return Err(e),
        }
    }

    Ok(DeleteManyResult {
        hard_deleted: hard_count,
        soft_deleted: soft_count,
        skipped: skipped_count,
        deleted_ids,
        upload_fields_to_clean,
    })
}
