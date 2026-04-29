//! Paginated find query with the full read lifecycle.

use crate::{
    core::{CollectionDefinition, Document},
    db::{AccessResult, Filter, FilterClause, FilterOp, FindQuery, query},
    service::{FindDocumentsInput, PaginatedResult, ServiceContext, ServiceError, helpers},
};

use super::post_process::post_process_docs;
use super::validate_filters::{validate_access_constraints, validate_user_filters};

type Result<T> = std::result::Result<T, ServiceError>;

/// Execute a paginated find query with the full read lifecycle.
///
/// Steps: validate user filters -> access check -> inject system filters ->
/// before_read -> find + count -> post-process -> build pagination.
/// Returns `PaginatedResult<Document>` with docs, total, and computed pagination metadata.
pub fn find_documents(
    ctx: &ServiceContext,
    input: &FindDocumentsInput,
) -> Result<PaginatedResult<Document>> {
    validate_user_filters(&input.query.filters)?;

    let resolved = ctx.resolve_conn()?;
    let conn = resolved.as_ref();
    let hooks = ctx.read_hooks()?;
    let def = ctx.collection_def();

    let access_ref = if input.trash {
        def.access.resolve_trash()
    } else {
        def.access.read.as_deref()
    };

    let access = hooks.check_access(access_ref, ctx.user, None, None)?;

    if matches!(access, AccessResult::Denied) {
        let msg = if input.trash {
            "Trash access denied"
        } else {
            "Read access denied"
        };
        return Err(ServiceError::AccessDenied(msg.into()));
    }

    let mut fq = build_effective_query(
        input.query,
        def,
        input.trash,
        input.include_drafts,
        input.status_filter.as_deref(),
    );

    // `_status` filtering is "happening" if the service-layer injected
    // either the published-only default OR an explicit user-supplied
    // status. Access-constraint hooks that mention `_status` are
    // permitted in either case (the SQL `_status` column is being
    // queried regardless of which value).
    let injecting_status = (input.status_filter.is_some()
        || (!input.include_drafts && def.has_drafts()))
        && def.has_drafts();

    if let AccessResult::Constrained(extra) = access {
        validate_access_constraints(&extra, input.trash, injecting_status, ctx.slug)?;
        fq.filters.extend(extra);
    }

    hooks.before_read(&def.hooks, ctx.slug, "find")?;

    let had_cursor = fq.after_cursor.is_some() || fq.before_cursor.is_some();
    let overfetch = input.cursor_enabled && had_cursor;

    if overfetch {
        fq.limit = fq.limit.map(|l| l + 1);
    }

    let mut docs = query::find(conn, ctx.slug, def, &fq, input.locale_ctx)?;

    let total = query::count_with_search(
        conn,
        ctx.slug,
        def,
        &fq.filters,
        input.locale_ctx,
        fq.search.as_deref(),
        fq.include_deleted,
    )?;

    // Restore original limit for pagination calculation.
    if overfetch {
        fq.limit = fq.limit.map(|l| l - 1);
    }

    let limit = fq.limit.unwrap_or(total);

    // Detect whether more pages exist via overfetch, then trim the extra doc.
    let cursor_has_more = if overfetch {
        if (docs.len() as i64) > limit {
            if fq.before_cursor.is_some() {
                docs.remove(0);
            } else {
                docs.pop();
            }
            Some(true)
        } else {
            Some(false)
        }
    } else {
        None
    };

    post_process_docs(ctx, conn, &mut docs, input);

    let pagination = helpers::build_pagination(helpers::PaginationInputs {
        docs: &docs,
        total,
        fq: &fq,
        cursor_enabled: input.cursor_enabled,
        has_timestamps: def.timestamps,
        has_drafts: def.has_drafts(),
        had_cursor,
        cursor_has_more,
    });

    Ok(PaginatedResult {
        docs,
        total,
        pagination,
    })
}

/// Clone the user-supplied query and inject service-owned system filters
/// (`_status` and `_deleted_at`) based on the typed flags.
///
/// Runs *after* `validate_user_filters` so the injected filters bypass the
/// system-column rule that user filters are subject to.
///
/// Status precedence: an explicit `status_filter` (translated by the admin
/// list handler from `?where[_status][equals]=X` URL params, including
/// OR-bucket forms) wins. One value injects `_status = X`; multiple
/// values widen to `_status IN (X, Y, …)`. Otherwise the
/// default-when-drafts rule fires: `include_drafts = false` &&
/// `def.has_drafts()` injects `_status = "published"`.
fn build_effective_query(
    user_query: &FindQuery,
    def: &CollectionDefinition,
    trash: bool,
    include_drafts: bool,
    status_filter: Option<&[String]>,
) -> FindQuery {
    let mut fq = user_query.clone();

    match status_filter {
        Some(values) if def.has_drafts() && !values.is_empty() => {
            let op = if values.len() == 1 {
                FilterOp::Equals(values[0].clone())
            } else {
                FilterOp::In(values.to_vec())
            };
            fq.filters.push(FilterClause::Single(Filter {
                field: "_status".to_string(),
                op,
            }));
        }
        _ if !include_drafts && def.has_drafts() => {
            fq.filters.push(FilterClause::Single(Filter {
                field: "_status".to_string(),
                op: FilterOp::Equals("published".to_string()),
            }));
        }
        _ => {}
    }

    if trash && def.soft_delete {
        fq.include_deleted = true;
        fq.filters.push(FilterClause::Single(Filter {
            field: "_deleted_at".to_string(),
            op: FilterOp::Exists,
        }));
    }

    fq
}
