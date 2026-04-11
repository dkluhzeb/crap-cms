//! Document counting with access control.

use crate::{
    db::{AccessResult, query},
    service::{CountDocumentsInput, ServiceContext, ServiceError},
};

type Result<T> = std::result::Result<T, ServiceError>;

/// Count documents matching the given filters, with access control.
///
/// Applies collection-level access check and merges any constraint filters.
pub fn count_documents(ctx: &ServiceContext, input: &CountDocumentsInput) -> Result<i64> {
    let resolved = ctx.resolve_conn()?;
    let conn = resolved.as_ref();
    let hooks = ctx.read_hooks()?;
    let def = ctx.collection_def();

    let access = hooks.check_access(def.access.read.as_deref(), ctx.user, None, None)?;

    if matches!(access, AccessResult::Denied) {
        return Err(ServiceError::AccessDenied("Read access denied".into()));
    }

    let mut merged = input.filters.to_vec();

    if let AccessResult::Constrained(extra) = access {
        merged.extend(extra);
    }

    let count = query::count_with_search(
        conn,
        ctx.slug,
        def,
        &merged,
        input.locale_ctx,
        input.search,
        input.include_deleted,
    )?;

    Ok(count)
}
