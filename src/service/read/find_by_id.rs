//! Single-document lookup by ID with the full read lifecycle.

use crate::{
    core::{CollectionDefinition, Document},
    db::{AccessResult, DbConnection, ops},
    service::{ServiceError, hooks::ReadHooks},
};

use super::{ReadOptions, post_process::post_process_single};

type Result<T> = std::result::Result<T, ServiceError>;

/// Look up a single document by ID with the full read lifecycle.
///
/// Steps: before_read -> find_by_id -> hydrate -> populate -> upload sizes ->
/// select strip -> field-level read strip -> after_read.
///
/// Returns `None` if the document doesn't exist (or is filtered by access constraints).
pub fn find_document_by_id(
    conn: &dyn DbConnection,
    hooks: &dyn ReadHooks,
    slug: &str,
    def: &CollectionDefinition,
    id: &str,
    opts: &ReadOptions,
) -> Result<Option<Document>> {
    // Collection-level access check -- short-circuit if denied
    let access = hooks.check_access(def.access.read.as_deref(), opts.user, Some(id), None)?;
    if matches!(access, AccessResult::Denied) {
        return Err(ServiceError::AccessDenied("Read access denied".into()));
    }

    // Merge caller-provided + access-derived constraints
    let constraints = match (opts.access_constraints.clone(), access) {
        (Some(mut existing), AccessResult::Constrained(extra)) => {
            existing.extend(extra);
            Some(existing)
        }
        (Some(existing), _) => Some(existing),
        (None, AccessResult::Constrained(extra)) => Some(extra),
        _ => None,
    };

    hooks.before_read(&def.hooks, slug, "find_by_id")?;

    let mut doc = match ops::find_by_id_full(
        conn,
        slug,
        def,
        id,
        opts.locale_ctx,
        constraints,
        opts.use_draft,
    )? {
        Some(d) => d,
        None => return Ok(None),
    };

    // Post-process (skip hydration -- find_by_id_full already handled it)
    post_process_single(conn, hooks, slug, def, &mut doc, opts, "find_by_id");

    Ok(Some(doc))
}
