//! Bulk update — update multiple documents matching a filter.

use anyhow::Context as _;

use super::bulk_access::{BulkScope, push_published_only_filter, scope_bulk_access};

use crate::service::OpDeadline;
use crate::{
    config::LocaleConfig,
    core::{DocumentFields, event::EventOperation},
    db::{FilterClause, FindQuery, LocaleContext, query},
    service::{
        ServiceContext, ServiceError, WriteInput, invalidate_user_streams_if_auth, run_pool_write,
        update_many_single_in_conn,
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
    /// Cooperative abort deadline, checked between documents (see
    /// [`OpDeadline`]).
    pub deadline: OpDeadline,
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
    // The whole bulk update runs in ONE transaction (the envelope) so it is
    // atomic: a per-document failure rolls the entire operation back. The
    // matching IDs are collected inside the transaction (projecting only
    // IDs, so the match-set is bounded by the ID-list size); unlike
    // DeleteMany, updated rows still match the filter, so the IDs must be
    // collected once up front.
    let (result, _) = run_pool_write(
        ctx,
        Some(opts.run_hooks),
        |inner| {
            let def = inner.collection_def()?;

            // Gate + scope the match-set at the chokepoint (see `bulk_access`).
            let mut scoped_filters = filters.to_vec();
            scope_bulk_access(
                inner,
                inner.write_hooks()?,
                &BulkScope {
                    operation: "update",
                    access_fn: def.access.update.as_ref(),
                    data: Some(data),
                    injecting_status: !opts.draft && def.has_drafts(),
                },
                &mut scoped_filters,
            )?;
            push_published_only_filter(def, opts.draft, &mut scoped_filters);

            let conn = inner.resolve_conn()?;
            let find_query = FindQuery::builder().filters(scoped_filters).build();
            let doc_ids =
                query::find_ids(conn.as_ref(), inner.slug, def, &find_query, opts.locale_ctx)
                    .context("Find matching IDs for update")?;

            enforce_bulk_limit("update_many", doc_ids.len(), opts.max_documents)?;

            let mut results = Vec::with_capacity(doc_ids.len());
            let mut ids = Vec::with_capacity(doc_ids.len());
            let mut modified = 0i64;

            for doc_id in &doc_ids {
                opts.deadline.check(modified)?;

                let input = WriteInput::builder(data.clone())
                    .locale_ctx(opts.locale_ctx)
                    .draft(opts.draft)
                    .ui_locale(opts.ui_locale.clone())
                    .build();

                // A failure here returns via `?`; the envelope rolls back
                // every change made so far.
                let (doc, _) = update_many_single_in_conn(inner, doc_id, input, locale_config)?;

                results.push((doc_id.clone(), doc.fields.clone()));
                ids.push(doc_id.clone());
                modified += 1;
            }

            // Final pre-commit check: everything after this returns into
            // the envelope's `tx.commit()`, so an expiry here still rolls back.
            opts.deadline.check(modified)?;

            Ok((
                UpdateManyResult {
                    modified,
                    updated_ids: ids,
                },
                results,
            ))
        },
        |ctx, (result, updated)| {
            // Per-doc events are gated by `ctx.emit_events` (set by the
            // surface; bulk defaults to off).
            for (id, fields) in updated {
                ctx.publish_mutation_event(EventOperation::Update, id, fields);
            }

            // A bulk role/group change on auth documents must tear down each
            // affected user's live-update streams, just like a single update.
            for id in &result.updated_ids {
                invalidate_user_streams_if_auth(ctx, id);
            }
        },
    )?;

    Ok(result)
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

    // Gate + scope the match-set at the chokepoint (see `bulk_access`).
    let mut scoped_filters = filters.to_vec();
    scope_bulk_access(
        ctx,
        ctx.write_hooks()?,
        &BulkScope {
            operation: "update",
            access_fn: def.access.update.as_ref(),
            data: Some(data),
            injecting_status: !opts.draft && def.has_drafts(),
        },
        &mut scoped_filters,
    )?;
    push_published_only_filter(def, opts.draft, &mut scoped_filters);

    let find_query = FindQuery::builder().filters(scoped_filters).build();

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
        opts.deadline.check(modified)?;
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

    // Parity with the single-document conn path (and the pool path's
    // orchestrator): every write clears the populate cache.
    ctx.clear_cache();

    // Final pre-commit check (see create_many).

    opts.deadline.check(modified)?;

    Ok(UpdateManyResult {
        modified,
        updated_ids,
    })
}
