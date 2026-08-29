//! Bulk delete — delete multiple documents matching a filter.

use anyhow::Context as _;

use crate::{
    config::LocaleConfig,
    core::DocumentFields,
    db::{FilterClause, FindQuery, query},
    service::{
        ServiceContext, ServiceError, delete_document_in_conn, invalidate_user_streams_if_auth,
        run_pool_write,
    },
};

use super::bulk_access::{delete_scope, scope_bulk_access};
use super::update_many::enforce_bulk_limit;

type Result<T> = std::result::Result<T, ServiceError>;

/// Result of a bulk delete operation.
#[derive(Debug)]
pub struct DeleteManyResult {
    pub hard_deleted: i64,
    pub soft_deleted: i64,
    pub skipped: i64,
    pub deleted_ids: Vec<String>,
    pub upload_fields_to_clean: Vec<DocumentFields>,
}

/// Options controlling bulk delete behavior.
pub struct DeleteManyOptions {
    /// Whether to run lifecycle hooks per document. Defaults to `true`.
    pub run_hooks: bool,
    /// Whether to include soft-deleted rows in the query. Required when
    /// emptying the trash (finding rows with `_deleted_at EXISTS`).
    pub include_deleted: bool,
    /// Maximum number of documents the operation may match before it is
    /// rejected (from `server.bulk_max_documents`). `0` = no limit.
    pub max_documents: i64,
}

impl Default for DeleteManyOptions {
    fn default() -> Self {
        Self {
            run_hooks: true,
            include_deleted: false,
            max_documents: 0,
        }
    }
}

/// Delete multiple documents matching the given filters.
///
/// **Pool mode** (`ctx.pool` set): collects the whole match-set up front (IDs
/// only) and deletes every document in a SINGLE transaction. The operation is
/// atomic: a real per-document failure rolls the whole batch back. Documents
/// blocked by an incoming reference are skipped individually (best-effort,
/// counted in `skipped`), not errored. Events, queued side-effects, and cache
/// invalidation run after the commit; upload files are returned for the caller
/// to delete post-commit (a crash then leaves orphaned files — safe — rather
/// than orphaned DB rows pointing at deleted files).
///
/// **Conn mode** (`ctx.conn` set, Lua path): runs on the caller's existing
/// transaction (already atomic with it), deleting each matching document in turn.
///
/// # Errors
///
/// Returns service-layer errors per-document or a backend error if the
/// find/delete queries fail.
pub fn delete_many(
    ctx: &ServiceContext,
    filters: &[FilterClause],
    locale_config: &LocaleConfig,
    opts: &DeleteManyOptions,
) -> Result<DeleteManyResult> {
    if ctx.pool.is_some() {
        delete_many_pool(ctx, filters, locale_config, opts)
    } else {
        delete_many_conn(ctx, filters, locale_config, opts)
    }
}

/// Pool-based bulk delete: collect the match-set, then delete every document in
/// one atomic transaction (commit once), publishing events after the commit.
fn delete_many_pool(
    ctx: &ServiceContext,
    filters: &[FilterClause],
    locale_config: &LocaleConfig,
    opts: &DeleteManyOptions,
) -> Result<DeleteManyResult> {
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
                &delete_scope(def),
                &mut scoped_filters,
            )?;

            // Collect the whole match-set up front (IDs only) so the entire
            // delete is atomic — a per-document failure rolls everything back.
            // Referenced documents are skipped individually (best-effort,
            // counted in `skipped`), not errored.
            let conn = inner.resolve_conn()?;
            let find_query = FindQuery::builder()
                .filters(scoped_filters)
                .include_deleted(opts.include_deleted)
                .build();
            let doc_ids = query::find_ids(conn.as_ref(), inner.slug, def, &find_query, None)
                .context("Find matching IDs for delete")?;

            enforce_bulk_limit("delete_many", doc_ids.len(), opts.max_documents)?;

            let mut hard_count = 0i64;
            let mut soft_count = 0i64;
            let mut skipped_count = 0i64;
            let mut upload_fields_to_clean = Vec::new();
            let mut deleted_ids = Vec::new();
            // Pre-deletion `_status` per deleted id, in lockstep with
            // `deleted_ids`, to gate each hard-delete event by the status view
            // the document was last in.
            let mut pre_statuses = Vec::new();

            for id in &doc_ids {
                match delete_document_in_conn(inner, id, Some(locale_config)) {
                    Ok(result) => {
                        if def.soft_delete {
                            soft_count += 1;
                        } else {
                            hard_count += 1;
                            if let Some(fields) = result.upload_doc_fields {
                                upload_fields_to_clean.push(fields);
                            }
                        }
                        deleted_ids.push(id.clone());
                        pre_statuses.push(result.pre_status);
                    }
                    // A referenced document is skipped (best-effort), not a failure.
                    Err(ServiceError::Referenced { .. }) => {
                        skipped_count += 1;
                    }
                    // A real error aborts the whole op via `?`; the envelope
                    // rolls back every delete made so far.
                    Err(e) => return Err(e),
                }
            }

            Ok((
                DeleteManyResult {
                    hard_deleted: hard_count,
                    soft_deleted: soft_count,
                    skipped: skipped_count,
                    deleted_ids,
                    upload_fields_to_clean,
                },
                pre_statuses,
            ))
        },
        |ctx, (result, pre_statuses)| {
            let soft_delete = ctx.collection_def().is_ok_and(|d| d.soft_delete);

            // Per-doc events are gated by `ctx.emit_events` (bulk defaults to off).
            for (id, pre_status) in result.deleted_ids.iter().zip(pre_statuses) {
                ctx.publish_delete_event(id, soft_delete, pre_status.clone());
            }
            // Deleting an auth document revokes that user — tear down each
            // affected user's live streams POST-COMMIT, for BOTH hard and soft
            // delete: the evaluator's `find_by_id` excludes soft-deleted rows,
            // so a trashed user is rejected on new requests and their open
            // streams must be closed too.
            for id in &result.deleted_ids {
                invalidate_user_streams_if_auth(ctx, id);
            }
        },
    )?;

    Ok(result)
}

/// Conn-based bulk delete: uses existing connection (Lua CRUD path).
fn delete_many_conn(
    ctx: &ServiceContext,
    filters: &[FilterClause],
    locale_config: &LocaleConfig,
    opts: &DeleteManyOptions,
) -> Result<DeleteManyResult> {
    let def = ctx.collection_def()?;

    // Gate + scope the match-set at the chokepoint (see `bulk_access`).
    let mut scoped_filters = filters.to_vec();
    scope_bulk_access(
        ctx,
        ctx.write_hooks()?,
        &delete_scope(def),
        &mut scoped_filters,
    )?;

    let find_query = FindQuery::builder()
        .filters(scoped_filters)
        .include_deleted(opts.include_deleted)
        .build();

    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();

    // Runs on the caller's existing connection/transaction (the Lua hook's
    // tx), so it is already atomic with that transaction. Collect IDs only
    // (bounded memory) and apply the same bulk limit as the pool path.
    let doc_ids = query::find_ids(conn, ctx.slug, def, &find_query, None)
        .context("Find matching IDs for delete")?;

    enforce_bulk_limit("delete_many", doc_ids.len(), opts.max_documents)?;

    let mut hard_count = 0i64;
    let mut soft_count = 0i64;
    let mut skipped_count = 0i64;
    let mut upload_fields_to_clean = Vec::new();
    let mut deleted_ids = Vec::new();

    for id in &doc_ids {
        match delete_document_in_conn(ctx, id, Some(locale_config)) {
            Ok(result) => {
                if def.soft_delete {
                    soft_count += 1;
                } else {
                    hard_count += 1;
                    if let Some(fields) = result.upload_doc_fields {
                        upload_fields_to_clean.push(fields);
                    }
                }

                // Gated by `ctx.emit_events`; in conn mode the enqueued event
                // flushes after the caller's tx commits.
                ctx.publish_delete_event(id, def.soft_delete, result.pre_status.clone());
                deleted_ids.push(id.clone());
            }
            Err(ServiceError::Referenced { .. }) => {
                skipped_count += 1;
            }
            Err(e) => return Err(e),
        }
    }

    // Deleting an auth document revokes that user — tear down each affected
    // user's live streams (conn mode fires immediate), for both hard and soft
    // delete. See the pool path for the soft-delete rationale.
    for id in &deleted_ids {
        invalidate_user_streams_if_auth(ctx, id);
    }

    Ok(DeleteManyResult {
        hard_deleted: hard_count,
        soft_deleted: soft_count,
        skipped: skipped_count,
        deleted_ids,
        upload_fields_to_clean,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SAFE-DEFAULT GUARD: bulk delete must run lifecycle hooks (access,
    /// hooks, ref-count decrement) per document by default, and must not
    /// include soft-deleted rows unless explicitly emptying the trash.
    #[test]
    fn delete_many_options_default_runs_hooks_and_excludes_deleted() {
        let opts = DeleteManyOptions::default();
        assert!(opts.run_hooks, "bulk delete must run hooks by default");
        assert!(
            !opts.include_deleted,
            "soft-deleted rows excluded by default"
        );
    }
}
