//! Document counting with access control.

use crate::{
    db::{Filter, FilterClause, FilterOp, LocaleContext, query},
    service::{
        CountDocumentsInput, ReadAccessCtx, ServiceContext, ServiceError, requested_views,
        resolve_trash_scope, resolve_view_scope,
    },
};

use super::validate_filters::validate_user_filters;

type Result<T> = std::result::Result<T, ServiceError>;

/// Count documents matching the given filters, with access control.
///
/// Steps: validate user filters -> resolve the access-scoped view (trash mode
/// or the published/draft union) -> count. Status visibility is composed by
/// [`ViewScope`](crate::db::ViewScope) exactly as in `find`.
///
/// # Errors
///
/// Returns service-layer errors (access denied, invalid filter) or a
/// backend error if the COUNT query fails.
pub fn count_documents(ctx: &ServiceContext, input: &CountDocumentsInput) -> Result<i64> {
    validate_user_filters(input.filters)?;

    let resolved = ctx.resolve_conn()?;
    let conn = resolved.as_ref();
    let hooks = ctx.read_hooks()?;
    let def = ctx.collection_def()?;

    let trash_active = input.trash && def.soft_delete;

    let read_ctx = ReadAccessCtx {
        def,
        slug: ctx.slug,
        user: ctx.user,
        id: None,
        locale: input.locale_ctx.map(LocaleContext::access_locale),
        operation: "count",
        ui_locale: None,
    };

    let mut merged = input.filters.to_vec();

    if trash_active {
        merged.extend(resolve_trash_scope(hooks, &read_ctx)?);
        merged.push(FilterClause::Single(Filter {
            field: "_deleted_at".to_string(),
            op: FilterOp::Exists,
        }));
    } else {
        let scope = resolve_view_scope(
            hooks,
            &read_ctx,
            requested_views(None, input.include_drafts),
        )?;
        if !scope.is_anything_visible() {
            return Err(ServiceError::AccessDenied("Read access denied".into()));
        }
        merged.extend(scope.into_filters());
    }

    let count = query::count_with_search(
        conn,
        ctx.slug,
        def,
        &merged,
        input.locale_ctx,
        input.search,
        trash_active,
    )?;

    Ok(count)
}
