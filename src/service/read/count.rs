//! Document counting with access control.

use crate::{
    core::{CollectionDefinition, Document},
    db::{AccessResult, DbConnection, FilterClause, LocaleContext, query},
    service::{ServiceError, hooks::ReadHooks},
};

type Result<T> = std::result::Result<T, ServiceError>;

/// Count documents matching the given filters, with access control.
///
/// Applies collection-level access check and merges any constraint filters.
#[allow(clippy::too_many_arguments)]
pub fn count_documents(
    conn: &dyn DbConnection,
    hooks: &dyn ReadHooks,
    slug: &str,
    def: &CollectionDefinition,
    filters: &[FilterClause],
    locale_ctx: Option<&LocaleContext>,
    search: Option<&str>,
    include_deleted: bool,
    user: Option<&Document>,
) -> Result<i64> {
    let access = hooks.check_access(def.access.read.as_deref(), user, None, None)?;
    if matches!(access, AccessResult::Denied) {
        return Err(ServiceError::AccessDenied("Read access denied".into()));
    }

    let mut merged = filters.to_vec();
    if let AccessResult::Constrained(extra) = access {
        merged.extend(extra);
    }

    let count = query::count_with_search(
        conn,
        slug,
        def,
        &merged,
        locale_ctx,
        search,
        include_deleted,
    )?;

    Ok(count)
}
