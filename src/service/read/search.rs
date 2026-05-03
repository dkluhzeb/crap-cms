//! Lightweight search for relationship fields.

use crate::{
    core::{Document, upload},
    db::{AccessResult, Filter, FilterClause, FilterOp, query},
    service::{PaginatedResult, SearchDocumentsInput, ServiceContext, ServiceError, helpers},
};

use super::validate_filters::validate_access_constraints;

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
    let injecting_status = !input.include_drafts && def.has_drafts();

    if let AccessResult::Constrained(extra) = access {
        // `_status` is allowed when the service is about to inject
        // `_status = "published"` (matches `find_documents`' behaviour);
        // otherwise `_status` from an access hook is rejected as a
        // probable typo. `_deleted_at` is always rejected here — search
        // never reaches trashed rows.
        validate_access_constraints(&extra, false, injecting_status, ctx.slug)?;
        fq.filters.extend(extra);
    }

    if injecting_status {
        fq.filters.push(FilterClause::Single(Filter {
            field: "_status".to_string(),
            op: FilterOp::Equals("published".to_string()),
        }));
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
    denied.extend(helpers::collect_api_hidden_field_names(&def.fields, ""));

    if !denied.is_empty() {
        for doc in &mut docs {
            doc.strip_fields(&denied);
        }
    }

    let pagination = helpers::build_pagination(helpers::PaginationInputs {
        docs: &docs,
        total,
        fq: &fq,
        cursor_enabled: input.cursor_enabled,
        has_timestamps: def.timestamps,
        has_drafts: def.has_drafts(),
        had_cursor,
        cursor_has_more,
    });

    Ok(PaginatedResult {
        docs,
        total,
        pagination,
    })
}
