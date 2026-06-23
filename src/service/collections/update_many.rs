//! Bulk update — update multiple documents matching a filter.

use std::{cell::RefCell, rc::Rc};

use anyhow::Context as _;

use crate::{
    config::LocaleConfig,
    core::{DocumentFields, event::EventOperation},
    db::{FilterClause, FindQuery, LocaleContext, query},
    hooks::LuaCrudInfra,
    service::{
        RunnerWriteHooks, ServiceContext, ServiceError, WriteInput, flush_queue,
        invalidate_user_streams_if_auth, update_many_single_in_conn,
    },
    typegen::lua::LuaAnnotation,
};

type Result<T> = std::result::Result<T, ServiceError>;

/// Result of a bulk update operation.
#[derive(Debug, crate::typegen::lua::LuaAnnotation)]
#[lua(class = "crap.UpdateManyResult")]
pub struct UpdateManyResult {
    /// Number of documents updated.
    pub modified: i64,
    /// Internal: IDs of the documents that were updated. Not surfaced
    /// to the Lua API.
    #[lua(skip)]
    pub updated_ids: Vec<String>,
}

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
    /// Maximum number of documents the operation may match before it is
    /// rejected (from `server.bulk_max_documents`). `0` = no limit.
    pub max_documents: i64,
}

/// Reject a bulk operation whose match-set exceeds `max_documents`
/// (`server.bulk_max_documents`). `0` disables the limit. Checked before
/// any write, so an over-limit operation changes nothing.
pub(super) fn enforce_bulk_limit(verb: &str, matched: usize, max_documents: i64) -> Result<()> {
    if max_documents > 0 && i64::try_from(matched).unwrap_or(i64::MAX) > max_documents {
        return Err(ServiceError::LimitExceeded(format!(
            "{verb} matched {matched} documents, exceeding the configured limit of \
             {max_documents} (server.bulk_max_documents). Narrow the filter or raise the limit."
        )));
    }
    Ok(())
}

/// Update multiple documents matching `filters` with the partial `data`.
///
/// **Pool mode** (`ctx.pool` set): collects every matching document ID up front
/// (projecting IDs only — updated rows still match the filter, so the match-set
/// must be gathered once rather than re-queried) and applies all per-document
/// updates in a SINGLE transaction. The operation is atomic: a failure on any
/// document rolls the whole batch back. Events, queued side-effects, and cache
/// invalidation run after the commit.
///
/// **Conn mode** (`ctx.conn` set, Lua path): runs on the caller's existing
/// transaction (already atomic with it), updating each matching document in turn.
///
/// # Errors
///
/// Returns service-layer errors per-document or a backend error if the
/// find/update queries fail.
pub fn update_many(
    ctx: &ServiceContext,
    filters: &[FilterClause],
    data: &DocumentFields,
    locale_config: &LocaleConfig,
    opts: &UpdateManyOptions<'_>,
) -> Result<UpdateManyResult> {
    if ctx.pool.is_some() {
        update_many_pool(ctx, filters, data, locale_config, opts)
    } else {
        update_many_conn(ctx, filters, data, locale_config, opts)
    }
}

/// Pool-based bulk update: collect matching IDs, then update them all in one
/// atomic transaction (commit once).
fn update_many_pool(
    ctx: &ServiceContext,
    filters: &[FilterClause],
    data: &DocumentFields,
    locale_config: &LocaleConfig,
    opts: &UpdateManyOptions<'_>,
) -> Result<UpdateManyResult> {
    let pool = ctx.pool.context("pool required")?;
    let runner = ctx.runner()?;
    let def = ctx.collection_def()?;

    // The whole bulk update runs in ONE transaction so it is atomic: a
    // per-document failure rolls the entire operation back rather than
    // leaving some rows changed. We open the write transaction up front and
    // collect the matching IDs inside it (projecting only IDs, not full
    // documents, so the match-set is bounded by the ID-list size). Unlike
    // DeleteMany — which re-queries because deleted rows leave the result
    // set — updated rows still match the filter, so the IDs must be
    // collected once; the per-document deep update then runs from the ID.
    let mut conn = pool.get().context("DB connection")?;
    let tx = conn
        .transaction_immediate()
        .context("Start bulk update transaction")?;

    let find_query = FindQuery::builder().filters(filters.to_vec()).build();
    let doc_ids = query::find_ids(&tx, ctx.slug, def, &find_query, opts.locale_ctx)
        .context("Find matching IDs for update")?;

    enforce_bulk_limit("update_many", doc_ids.len(), opts.max_documents)?;

    let queue = Rc::new(RefCell::new(Vec::new()));
    let infra = LuaCrudInfra::from_ctx(ctx, Some(queue.clone()), None);

    let mut wh = RunnerWriteHooks::new(runner)
        .with_hooks_enabled(opts.run_hooks)
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
        .event_transport(ctx.event_transport.clone())
        .cache(ctx.cache.clone())
        .event_queue(queue.clone())
        .build();

    let mut results = Vec::with_capacity(doc_ids.len());
    let mut ids = Vec::with_capacity(doc_ids.len());
    let mut modified = 0i64;

    for doc_id in &doc_ids {
        let input = WriteInput::builder(data.clone())
            .locale_ctx(opts.locale_ctx)
            .draft(opts.draft)
            .ui_locale(opts.ui_locale.clone())
            .build();

        // A failure here returns via `?`; `tx` is dropped without commit,
        // rolling back every change made so far.
        let (doc, _) = update_many_single_in_conn(&inner_ctx, doc_id, input, locale_config)?;

        results.push((doc_id.clone(), doc.fields.clone()));
        ids.push(doc_id.clone());
        modified += 1;
    }

    // Release the borrows of `tx` before committing it.
    drop(inner_ctx);
    drop(wh);

    tx.commit().context("Commit bulk update transaction")?;

    ctx.clear_cache();

    // Per-doc events are gated by `ctx.emit_events` (set by the surface;
    // bulk defaults to off). Nested-hook events queued during the op always
    // flush.
    for (id, fields) in &results {
        ctx.publish_mutation_event(EventOperation::Update, id, fields);
    }

    // A bulk role/group change on auth documents must tear down each affected
    // user's live-update streams, just like a single update (shared helper —
    // no-op for non-auth collections or without an invalidation transport).
    for id in &ids {
        invalidate_user_streams_if_auth(ctx, id);
    }

    flush_queue(ctx, &queue);

    Ok(UpdateManyResult {
        modified,
        updated_ids: ids,
    })
}

/// Conn-based bulk update: uses existing connection (Lua CRUD path).
fn update_many_conn(
    ctx: &ServiceContext,
    filters: &[FilterClause],
    data: &DocumentFields,
    locale_config: &LocaleConfig,
    opts: &UpdateManyOptions<'_>,
) -> Result<UpdateManyResult> {
    let def = ctx.collection_def()?;

    let find_query = FindQuery::builder().filters(filters.to_vec()).build();

    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();

    // Runs on the caller's existing connection/transaction (the Lua hook's
    // tx), so it is already atomic with that transaction. Collect IDs only
    // (bounded memory) and apply the same bulk limit as the pool path.
    let doc_ids = query::find_ids(conn, ctx.slug, def, &find_query, opts.locale_ctx)
        .context("Find matching IDs for update")?;

    enforce_bulk_limit("update_many", doc_ids.len(), opts.max_documents)?;

    let mut updated_ids = Vec::with_capacity(doc_ids.len());
    let mut modified = 0i64;

    for doc_id in &doc_ids {
        let input = WriteInput::builder(data.clone())
            .locale_ctx(opts.locale_ctx)
            .draft(opts.draft)
            .ui_locale(opts.ui_locale.clone())
            .build();

        let (updated_doc, _) = update_many_single_in_conn(ctx, doc_id, input, locale_config)?;

        // Gated by `ctx.emit_events`; in conn mode the enqueued event flushes
        // after the caller's tx commits.
        ctx.publish_mutation_event(EventOperation::Update, doc_id, &updated_doc.fields);
        updated_ids.push(doc_id.clone());
        modified += 1;
    }

    for id in &updated_ids {
        invalidate_user_streams_if_auth(ctx, id);
    }

    Ok(UpdateManyResult {
        modified,
        updated_ids,
    })
}
