//! Paginated find query with the full read lifecycle.

use crate::{
    core::{CollectionDefinition, Document},
    db::{AccessResult, DbConnection, FindQuery, query},
    service::{ServiceError, hooks::ReadHooks},
};

use super::{ReadOptions, post_process::post_process_docs};

type Result<T> = std::result::Result<T, ServiceError>;

/// Result of a find operation.
pub struct FindResult {
    pub docs: Vec<Document>,
    pub total: i64,
}

/// Execute a paginated find query with the full read lifecycle.
///
/// Steps: before_read -> find + count -> hydrate -> populate -> upload sizes ->
/// select strip -> field-level read strip -> after_read.
///
/// Access control (constraint filters) must be pre-applied by the caller
/// into `find_query.filters`.
pub fn find_documents(
    conn: &dyn DbConnection,
    hooks: &dyn ReadHooks,
    slug: &str,
    def: &CollectionDefinition,
    find_query: &FindQuery,
    opts: &ReadOptions,
) -> Result<FindResult> {
    // Collection-level access check -- short-circuit if denied
    let access = hooks.check_access(def.access.read.as_deref(), opts.user, None, None)?;
    if matches!(access, AccessResult::Denied) {
        return Err(ServiceError::AccessDenied("Read access denied".into()));
    }

    // Merge access constraints into filters
    let mut fq = find_query.clone();
    if let AccessResult::Constrained(extra) = access {
        fq.filters.extend(extra);
    }

    hooks.before_read(&def.hooks, slug, "find")?;

    let mut docs = query::find(conn, slug, def, &fq, opts.locale_ctx)?;
    let total = query::count_with_search(
        conn,
        slug,
        def,
        &fq.filters,
        opts.locale_ctx,
        fq.search.as_deref(),
        fq.include_deleted,
    )?;

    post_process_docs(conn, hooks, slug, def, &mut docs, opts, "find");

    Ok(FindResult { docs, total })
}
