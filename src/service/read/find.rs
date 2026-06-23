//! Paginated find query with the full read lifecycle.

use crate::{
    core::{CollectionDefinition, Document},
    db::{Filter, FilterClause, FilterOp, LocaleContext, query},
    service::{
        FindDocumentsInput, PaginatedResult, ReadAccessCtx, ReadHooks, ServiceContext,
        ServiceError, helpers, requested_views, resolve_trash_scope, resolve_view_scope,
    },
};

use super::post_process::post_process_docs;
use super::validate_filters::validate_user_filters;

type Result<T> = std::result::Result<T, ServiceError>;

/// Resolve the access-scoped filters to AND into a find.
///
/// Two independent axes: the **lifecycle** axis (live vs trash) and, within the
/// live view, the **status** union (published vs draft). Trash is a distinct
/// mode gated by `access.trash`; the live view is the published/draft union
/// resolved through [`ViewScope`](crate::db::ViewScope), which downgrades denied
/// views away rather than erroring.
fn scoped_read_filters(
    hooks: &dyn ReadHooks,
    ctx: &ServiceContext,
    def: &CollectionDefinition,
    input: &FindDocumentsInput,
) -> Result<Vec<FilterClause>> {
    let read_ctx = ReadAccessCtx {
        def,
        slug: ctx.slug,
        user: ctx.user,
        id: None,
        locale: input.locale_ctx.map(LocaleContext::access_locale),
        operation: "find",
        ui_locale: None,
    };

    if input.trash && def.soft_delete {
        return resolve_trash_scope(hooks, &read_ctx);
    }

    let scope = resolve_view_scope(
        hooks,
        &read_ctx,
        requested_views(input.status_filter.as_deref(), input.include_drafts),
    )?;

    if !scope.is_anything_visible() {
        return Err(ServiceError::AccessDenied("Read access denied".into()));
    }

    Ok(scope.into_filters())
}

/// Execute a paginated find query with the full read lifecycle.
///
/// Steps: validate user filters -> access check -> inject system filters ->
/// `before_read` -> find + count -> post-process -> build pagination.
/// Returns `PaginatedResult<Document>` with docs, total, and computed pagination metadata.
///
/// # Errors
///
/// Returns service-layer errors (access denied, invalid filter, hook errors)
/// or a backend error if the find/count queries fail.
pub fn find_documents(
    ctx: &ServiceContext,
    input: &FindDocumentsInput,
) -> Result<PaginatedResult<Document>> {
    validate_user_filters(&input.query.filters)?;

    let resolved = ctx.resolve_conn()?;
    let conn = resolved.as_ref();
    let hooks = ctx.read_hooks()?;
    let def = ctx.collection_def()?;

    let mut fq = input.query.clone();
    fq.filters
        .extend(scoped_read_filters(hooks, ctx, def, input)?);

    // Trash is a lifecycle mode orthogonal to the status union: restrict to
    // soft-deleted rows once `scoped_read_filters` has cleared `access.trash`.
    if input.trash && def.soft_delete {
        fq.include_deleted = true;
        fq.filters.push(FilterClause::Single(Filter {
            field: "_deleted_at".to_string(),
            op: FilterOp::Exists,
        }));
    }

    let req_context = hooks.before_read(
        &def.hooks,
        ctx.slug,
        "find",
        input.locale_ctx.map(LocaleContext::access_locale),
    )?;

    let overfetch = helpers::begin_cursor_overfetch(&mut fq, input.cursor_enabled);

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

    let cursor_has_more = helpers::finish_cursor_overfetch(&mut fq, &mut docs, overfetch, total);

    post_process_docs(ctx, conn, &mut docs, input, req_context);

    let pagination = helpers::build_pagination(&helpers::PaginationInputs {
        docs: &docs,
        total,
        fq: &fq,
        cursor_enabled: input.cursor_enabled,
        has_timestamps: def.timestamps,
        has_drafts: def.has_drafts(),
        cursor_has_more,
    });

    Ok(PaginatedResult {
        docs,
        total,
        pagination,
    })
}
