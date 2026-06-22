//! Lightweight search for relationship fields.

use super::validate_filters::validate_user_filters;
use crate::{
    core::{Document, upload},
    db::{LocaleContext, query},
    service::{
        PaginatedResult, ReadAccessCtx, SearchDocumentsInput, ServiceContext, ServiceError,
        helpers, requested_views, resolve_view_scope,
    },
};

type Result<T> = std::result::Result<T, ServiceError>;

/// Lightweight search for relationship fields — access check + find + count +
/// upload sizes + field stripping + pagination.
///
/// Unlike `find_documents`, this skips hooks, hydration, and population.
/// Used by the admin relationship search API.
///
/// # Errors
///
/// Returns service-layer errors (access denied) or a backend error if the
/// find/count queries fail.
pub fn search_documents(
    ctx: &ServiceContext,
    input: &SearchDocumentsInput,
) -> Result<PaginatedResult<Document>> {
    let resolved = ctx.resolve_conn()?;
    let conn = resolved.as_ref();
    let hooks = ctx.read_hooks()?;
    let def = ctx.collection_def()?;

    // Reject user filters on system columns (`_status`/`_deleted_at`/…) before
    // the engine composes its own view filters — parity with `find_documents`
    // so no search caller can slip a status/lifecycle filter past the chokepoint.
    validate_user_filters(&input.query.filters)?;

    // Status visibility is the published/draft union, gated per view; a reader
    // who can see nothing gets an empty result (search downgrades, never errors).
    // Search has no trash mode — it never reaches soft-deleted rows.
    let scope = resolve_view_scope(
        hooks,
        &ReadAccessCtx {
            def,
            slug: ctx.slug,
            user: ctx.user,
            id: None,
            locale: input.locale_ctx.map(LocaleContext::access_locale),
            operation: "search",
            ui_locale: None,
        },
        requested_views(None, input.include_drafts),
    )?;

    if !scope.is_anything_visible() {
        return Ok(PaginatedResult::default());
    }

    let mut fq = input.query.clone();
    fq.filters.extend(scope.into_filters());

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

    // Saturate doc count for the unreachable case so the > check still works.
    let docs_count = i64::try_from(docs.len()).unwrap_or(i64::MAX);
    let cursor_has_more = if overfetch {
        if docs_count > limit {
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

    // Field-read access is data-aware (per-doc, per-row); the API-hidden set is
    // document-independent and computed once.
    let access_locale = input.locale_ctx.map(LocaleContext::access_locale);
    let api_hidden = helpers::collect_api_hidden_field_names(&def.fields, "");

    for doc in &mut docs {
        hooks.strip_read_access_doc(&def.fields, doc, ctx.user, access_locale);

        if !api_hidden.is_empty() {
            doc.strip_fields(&api_hidden);
        }
    }

    let pagination = helpers::build_pagination(&helpers::PaginationInputs {
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
