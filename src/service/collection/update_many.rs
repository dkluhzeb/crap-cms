//! Bulk update — update multiple documents matching a filter.

use std::{cell::RefCell, collections::HashMap, rc::Rc};

use anyhow::Context as _;
use serde_json::Value;

use crate::{
    config::LocaleConfig,
    core::event::EventOperation,
    db::{FilterClause, FindQuery, LocaleContext, query},
    service::{
        RunnerWriteHooks, ServiceContext, ServiceError, WriteInput, flush_queue,
        update_many_single_core,
    },
};

const CHUNK_SIZE: usize = 500;

type Result<T> = std::result::Result<T, ServiceError>;

/// Result of a bulk update operation.
pub struct UpdateManyResult {
    pub modified: i64,
    pub updated_ids: Vec<String>,
}

/// Update multiple documents matching the given filters.
///
/// **Pool mode** (`ctx.pool` set): two-phase approach — first find all matching
/// document IDs (updated docs still match the filter, so IDs must be collected
/// upfront to avoid infinite re-processing), then update in chunked transactions.
///
/// **Conn mode** (`ctx.conn` set, Lua path): finds all matching docs on the
/// existing connection and updates them one by one.
///
/// Options controlling bulk update behavior.
pub struct UpdateManyOptions<'a> {
    /// Locale context for the update.
    pub locale_ctx: Option<&'a LocaleContext>,
    /// Whether to run lifecycle hooks per document.
    pub run_hooks: bool,
    /// Whether to target draft versions.
    pub draft: bool,
    /// UI locale string for hook context.
    pub ui_locale: Option<String>,
}

pub fn update_many(
    ctx: &ServiceContext,
    filters: Vec<FilterClause>,
    data: HashMap<String, String>,
    join_data: &HashMap<String, Value>,
    locale_config: &LocaleConfig,
    opts: &UpdateManyOptions<'_>,
) -> Result<UpdateManyResult> {
    if ctx.pool.is_some() {
        update_many_pool(ctx, filters, data, join_data, locale_config, opts)
    } else {
        update_many_conn(ctx, filters, data, join_data, locale_config, opts)
    }
}

/// Pool-based bulk update: phase 1 finds IDs, phase 2 updates in chunks.
fn update_many_pool(
    ctx: &ServiceContext,
    filters: Vec<FilterClause>,
    data: HashMap<String, String>,
    join_data: &HashMap<String, Value>,
    locale_config: &LocaleConfig,
    opts: &UpdateManyOptions<'_>,
) -> Result<UpdateManyResult> {
    let pool = ctx.pool.context("pool required")?;
    let runner = ctx.runner()?;
    let def = ctx.collection_def();

    // Phase 1: Find all matching doc IDs in a single read transaction.
    // Unlike DeleteMany (which re-queries because deleted docs leave the
    // result set), updated docs still match the same filter, so we must
    // collect IDs upfront to avoid re-updating in an infinite loop.
    let doc_ids = {
        let mut conn = pool.get().context("DB connection")?;
        let tx = conn
            .transaction_immediate()
            .context("Start read transaction")?;

        let find_query = FindQuery::builder().filters(filters).build();

        let docs = query::find(&tx, ctx.slug, def, &find_query, opts.locale_ctx)
            .context("Find docs for update")?;

        tx.commit().context("Commit read transaction")?;

        docs.into_iter().map(|d| d.id).collect::<Vec<_>>()
    };

    // Phase 2: Update in chunks to keep transactions short.
    let mut count = 0i64;
    let mut ids = Vec::new();

    for chunk in doc_ids.chunks(CHUNK_SIZE) {
        let mut conn = pool.get().context("DB connection")?;
        let tx = conn
            .transaction_immediate()
            .context("Start update transaction")?;

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
            .event_transport(ctx.event_transport.clone())
            .cache(ctx.cache.clone())
            .event_queue(queue.clone())
            .build();

        let mut chunk_results = Vec::with_capacity(chunk.len());

        for doc_id in chunk {
            let input = WriteInput::builder(data.clone(), join_data)
                .locale_ctx(opts.locale_ctx)
                .draft(opts.draft)
                .ui_locale(opts.ui_locale.clone())
                .build();

            let (doc, _) = update_many_single_core(&inner_ctx, doc_id, input, locale_config)?;

            chunk_results.push((doc_id.to_string(), doc.fields.clone()));
            ids.push(doc_id.to_string());
            count += 1;
        }

        drop(inner_ctx);

        tx.commit().context("Commit update transaction")?;

        ctx.clear_cache();

        // Publish events for this chunk after commit.
        for (id, fields) in chunk_results {
            ctx.publish_mutation_event(EventOperation::Update, &id, fields);
        }
        flush_queue(ctx, &queue);
    }

    Ok(UpdateManyResult {
        modified: count,
        updated_ids: ids,
    })
}

/// Conn-based bulk update: uses existing connection (Lua CRUD path).
fn update_many_conn(
    ctx: &ServiceContext,
    filters: Vec<FilterClause>,
    data: HashMap<String, String>,
    join_data: &HashMap<String, Value>,
    locale_config: &LocaleConfig,
    opts: &UpdateManyOptions<'_>,
) -> Result<UpdateManyResult> {
    let def = ctx.collection_def();

    let find_query = FindQuery::builder().filters(filters).build();

    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();

    let docs = query::find(conn, ctx.slug, def, &find_query, opts.locale_ctx)
        .context("Find docs for update")?;

    let mut modified = 0i64;
    let mut updated_ids = Vec::new();

    for doc in &docs {
        let input = WriteInput::builder(data.clone(), join_data)
            .locale_ctx(opts.locale_ctx)
            .draft(opts.draft)
            .ui_locale(opts.ui_locale.clone())
            .build();

        let (updated_doc, _) = update_many_single_core(ctx, &doc.id, input, locale_config)?;

        ctx.publish_mutation_event(EventOperation::Update, &doc.id, updated_doc.fields.clone());
        updated_ids.push(doc.id.to_string());
        modified += 1;
    }

    Ok(UpdateManyResult {
        modified,
        updated_ids,
    })
}
