//! Paginated find query with the full read lifecycle.

use crate::{
    core::Document,
    db::{AccessResult, query},
    service::{FindDocumentsInput, PaginatedResult, ServiceContext, ServiceError, helpers},
};

use super::post_process::post_process_docs;

type Result<T> = std::result::Result<T, ServiceError>;

/// Execute a paginated find query with the full read lifecycle.
///
/// Steps: access check -> before_read -> find + count -> post-process -> build pagination.
/// Returns `PaginatedResult<Document>` with docs, total, and computed pagination metadata.
pub fn find_documents(
    ctx: &ServiceContext,
    input: &FindDocumentsInput,
) -> Result<PaginatedResult<Document>> {
    let resolved = ctx.resolve_conn()?;
    let conn = resolved.as_ref();
    let hooks = ctx.read_hooks()?;
    let def = ctx.collection_def();

    let access_ref = if input.trash {
        def.access.resolve_trash()
    } else {
        def.access.read.as_deref()
    };

    let access = hooks.check_access(access_ref, ctx.user, None, None)?;

    if matches!(access, AccessResult::Denied) {
        let msg = if input.trash {
            "Trash access denied"
        } else {
            "Read access denied"
        };
        return Err(ServiceError::AccessDenied(msg.into()));
    }

    let mut fq = input.query.clone();

    if let AccessResult::Constrained(extra) = access {
        fq.filters.extend(extra);
    }

    hooks.before_read(&def.hooks, ctx.slug, "find")?;

    let had_cursor = fq.after_cursor.is_some() || fq.before_cursor.is_some();
    let overfetch = input.cursor_enabled && had_cursor;

    if overfetch {
        fq.limit = fq.limit.map(|l| l + 1);
    }

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

    // Restore original limit for pagination calculation.
    if overfetch {
        fq.limit = fq.limit.map(|l| l - 1);
    }

    let limit = fq.limit.unwrap_or(total);

    // Detect whether more pages exist via overfetch, then trim the extra doc.
    let cursor_has_more = if overfetch {
        if (docs.len() as i64) > limit {
            if fq.before_cursor.is_some() {
                docs.remove(0);
            } else {
                docs.pop();
            }
            Some(true)
        } else {
            Some(false)
        }
    } else {
        None
    };

    post_process_docs(ctx, conn, &mut docs, input);

    let pagination = helpers::build_pagination(
        &docs,
        total,
        &fq,
        input.cursor_enabled,
        def.timestamps,
        had_cursor,
        cursor_has_more,
    );

    Ok(PaginatedResult {
        docs,
        total,
        pagination,
    })
}
