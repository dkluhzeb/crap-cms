//! Lightweight search for relationship fields.

use crate::{
    core::{Document, upload},
    db::{AccessResult, query},
    service::{PaginatedResult, SearchDocumentsInput, ServiceContext, ServiceError, helpers},
};

type Result<T> = std::result::Result<T, ServiceError>;

/// Lightweight search for relationship fields — access check + find + count +
/// upload sizes + field stripping + pagination.
///
/// Unlike `find_documents`, this skips hooks, hydration, and population.
/// Used by the admin relationship search API.
pub fn search_documents(
    ctx: &ServiceContext,
    input: &SearchDocumentsInput,
) -> Result<PaginatedResult<Document>> {
    let resolved = ctx.resolve_conn()?;
    let conn = resolved.as_ref();
    let hooks = ctx.read_hooks()?;
    let def = ctx.collection_def();

    let access = hooks.check_access(def.access.read.as_deref(), ctx.user, None, None)?;

    if matches!(access, AccessResult::Denied) {
        return Ok(PaginatedResult::default());
    }

    let mut fq = input.query.clone();

    if let AccessResult::Constrained(extra) = access {
        fq.filters.extend(extra);
    }

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

    if overfetch {
        fq.limit = fq.limit.map(|l| l - 1);
    }

    let limit = fq.limit.unwrap_or(total);

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

    if let Some(ref uc) = def.upload
        && uc.enabled
    {
        for doc in &mut docs {
            upload::assemble_sizes_object(doc, uc);
        }
    }

    let mut denied = hooks.field_read_denied(&def.fields, ctx.user);
    denied.extend(helpers::collect_hidden_field_names(&def.fields, ""));

    if !denied.is_empty() {
        for doc in &mut docs {
            doc.strip_fields(&denied);
        }
    }

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
