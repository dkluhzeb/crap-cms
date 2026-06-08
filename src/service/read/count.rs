//! Document counting with access control.

use crate::{
    db::{AccessResult, Filter, FilterClause, FilterOp, LocaleContext, query},
    hooks::AccessCheckInput,
    service::{CountDocumentsInput, ServiceContext, ServiceError},
};

use super::validate_filters::{validate_access_constraints, validate_user_filters};

type Result<T> = std::result::Result<T, ServiceError>;

/// Count documents matching the given filters, with access control.
///
/// Steps: validate user filters -> access check -> inject system filters
/// (`_status`/`_deleted_at`) -> count.
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

    let access = hooks.check_access(&AccessCheckInput {
        access_ref: def.access.read.as_deref(),
        user: ctx.user,
        id: None,
        data: None,
        locale: input.locale_ctx.map(LocaleContext::access_locale),
        operation: "count",
        collection: ctx.slug,
        ui_locale: None,
    })?;

    if matches!(access, AccessResult::Denied) {
        return Err(ServiceError::AccessDenied("Read access denied".into()));
    }

    let mut merged = input.filters.to_vec();

    let injecting_status = !input.include_drafts && def.has_drafts();

    if injecting_status {
        merged.push(FilterClause::Single(Filter {
            field: "_status".to_string(),
            op: FilterOp::Equals("published".to_string()),
        }));
    }

    let trash_active = input.trash && def.soft_delete;

    if trash_active {
        merged.push(FilterClause::Single(Filter {
            field: "_deleted_at".to_string(),
            op: FilterOp::Exists,
        }));
    }

    if let AccessResult::Constrained(extra) = access {
        validate_access_constraints(&extra, trash_active, injecting_status, ctx.slug)?;
        merged.extend(extra);
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
