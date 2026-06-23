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

    let overfetch = helpers::begin_cursor_overfetch(&mut fq, input.cursor_enabled);

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

    let cursor_has_more = helpers::finish_cursor_overfetch(&mut fq, &mut docs, overfetch, total);

    if let Some(ref uc) = def.upload
        && uc.enabled
    {
        for doc in &mut docs {
            upload::assemble_sizes_object(doc, uc);
        }
    }

    // Field-read access is data-aware (per-doc, per-row), stripped in ONE batch
    // so the Lua VM is acquired once for the whole list. The API-hidden set is
    // document-independent and computed once.
    let access_locale = input.locale_ctx.map(LocaleContext::access_locale);
    let api_hidden = helpers::collect_api_hidden_field_names(&def.fields, "");

    hooks.strip_read_access_docs(&def.fields, &mut docs, ctx.slug, ctx.user, access_locale);

    if !api_hidden.is_empty() {
        for doc in &mut docs {
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
        cursor_has_more,
    });

    Ok(PaginatedResult {
        docs,
        total,
        pagination,
    })
}
