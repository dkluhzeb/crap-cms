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

    let trash_active = input.trash && def.soft_delete;
    let wants_draft = input.include_drafts && def.has_drafts();

    // Counting trash / drafts is gated at edit level (resolve_trash /
    // resolve_draft), not by `access.read` — a plain reader must not be able to
    // enumerate (or count) unpublished or trashed content. Matches `find`.
    let access_ref = if trash_active {
        def.access.resolve_trash()
    } else if wants_draft {
        def.access.resolve_draft()
    } else {
        def.access.read.as_ref()
    };

    let access = hooks.check_access(&AccessCheckInput {
        access: access_ref,
        user: ctx.user,
        id: None,
        data: None,
        locale: input.locale_ctx.map(LocaleContext::access_locale),
        operation: "count",
        collection: ctx.slug,
        ui_locale: None,
    })?;

    if matches!(access, AccessResult::Denied) {
        let msg = if trash_active {
            "Trash access denied"
        } else if wants_draft {
            "Draft access denied"
        } else {
            "Read access denied"
        };
        return Err(ServiceError::AccessDenied(msg.into()));
    }

    let mut merged = input.filters.to_vec();

    let injecting_status = !input.include_drafts && def.has_drafts();

    if injecting_status {
        merged.push(FilterClause::Single(Filter {
            field: "_status".to_string(),
            op: FilterOp::Equals("published".to_string()),
        }));
    }

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
