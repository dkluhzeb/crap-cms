//! Single-document lookup by ID with the full read lifecycle.

use crate::{
    core::Document,
    db::{AccessResult, ops},
    service::{FindByIdInput, ServiceContext, ServiceError},
};

use super::post_process::post_process_single;
use super::validate_filters::validate_access_constraints;

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

    let access_ref = if input.include_deleted {
        def.access.resolve_trash()
    } else {
        def.access.read.as_deref()
    };

    let access = hooks.check_access(access_ref, ctx.user, Some(input.id), None)?;

    if matches!(access, AccessResult::Denied) {
        let msg = if input.include_deleted {
            "Trash access denied"
        } else {
            "Read access denied"
        };
        return Err(ServiceError::AccessDenied(msg.into()));
    }

    // find_by_id has no draft-filtering flag of its own: when `use_draft` is
    // false on a drafts-enabled collection the service is effectively reading
    // the published snapshot, so access-hook filters on `_status` are allowed.
    let injecting_status = !input.use_draft && def.has_drafts();

    let constraints = match (input.access_constraints.clone(), access) {
        (Some(mut existing), AccessResult::Constrained(extra)) => {
            validate_access_constraints(&extra, input.include_deleted, injecting_status, ctx.slug)?;
            existing.extend(extra);
            Some(existing)
        }
        (Some(existing), _) => Some(existing),
        (None, AccessResult::Constrained(extra)) => {
            validate_access_constraints(&extra, input.include_deleted, injecting_status, ctx.slug)?;
            Some(extra)
        }
        _ => None,
    };

    hooks.before_read(&def.hooks, ctx.slug, "find_by_id")?;

    let mut doc = match ops::find_by_id_full(ops::FindByIdFullParams {
        conn,
        slug: ctx.slug,
        def,
        id: input.id,
        locale_ctx: input.locale_ctx,
        constraints,
        use_draft: input.use_draft,
        include_deleted: input.include_deleted,
    })? {
        Some(d) => d,
        None => return Ok(None),
    };

    post_process_single(ctx, conn, &mut doc, input, "find_by_id");

    Ok(Some(doc))
}
