//! Single-document lookup by ID with the full read lifecycle.

use crate::{
    core::Document,
    db::{AccessResult, ops},
    service::{FindByIdInput, ServiceContext, ServiceError},
};

use super::post_process::post_process_single;

type Result<T> = std::result::Result<T, ServiceError>;

/// Look up a single document by ID with the full read lifecycle.
///
/// Steps: access check -> before_read -> find_by_id -> post-process.
pub fn find_document_by_id(
    ctx: &ServiceContext,
    input: &FindByIdInput,
) -> Result<Option<Document>> {
    let resolved = ctx.resolve_conn()?;
    let conn = resolved.as_ref();
    let hooks = ctx.read_hooks()?;
    let def = ctx.collection_def();

    let access = hooks.check_access(def.access.read.as_deref(), ctx.user, Some(input.id), None)?;

    if matches!(access, AccessResult::Denied) {
        return Err(ServiceError::AccessDenied("Read access denied".into()));
    }

    let constraints = match (input.access_constraints.clone(), access) {
        (Some(mut existing), AccessResult::Constrained(extra)) => {
            existing.extend(extra);
            Some(existing)
        }
        (Some(existing), _) => Some(existing),
        (None, AccessResult::Constrained(extra)) => Some(extra),
        _ => None,
    };

    hooks.before_read(&def.hooks, ctx.slug, "find_by_id")?;

    let mut doc = match ops::find_by_id_full(
        conn,
        ctx.slug,
        def,
        input.id,
        input.locale_ctx,
        constraints,
        input.use_draft,
    )? {
        Some(d) => d,
        None => return Ok(None),
    };

    post_process_single(ctx, conn, &mut doc, input, "find_by_id");

    Ok(Some(doc))
}
