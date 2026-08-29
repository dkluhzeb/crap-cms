//! The `find` (list) operation.

use crate::{
    core::Document,
    db::{FindQuery, LocaleContext, query, query::filter::normalize_filter_fields},
    service::{
        FindDocumentsInput, PaginatedResult, ServiceContext, ServiceError, find_documents,
        validate_user_filters,
    },
};

use crate::core::Builder;

use super::Operation;

/// Owned arguments for [`Find`]. The query arrives already decoded into the
/// canonical [`FindQuery`] (filters in the shared `FilterOp` grammar);
/// decoding from the wire shape is the codec's job. Definition-dependent
/// behavior — the trash downgrade and the trash default sort order — lives
/// in [`Find::run`], previously quadruplicated across the four surfaces.
#[derive(Builder)]
pub struct FindArgs {
    #[builder(required)]
    pub query: FindQuery,
    pub depth: i32,
    /// Hydrate join-table fields (arrays/blocks/has-many). The admin list
    /// view opts out; API surfaces hydrate.
    #[builder(default = true)]
    pub hydrate: bool,
    pub locale_ctx: Option<LocaleContext>,
    pub include_drafts: bool,
    /// Explicit `_status` view filter (admin list filter builder).
    pub status_filter: Option<Vec<String>>,
    pub cursor_enabled: bool,
    /// List the trash view. Ignored unless the collection has soft delete.
    pub trash: bool,
}

/// Paginated list query with the full read lifecycle.
pub enum Find {}

impl Operation for Find {
    type Args = FindArgs;
    type Output = PaginatedResult<Document>;

    const NAME: &'static str = "find";

    fn run(ctx: &ServiceContext<'_>, args: Self::Args) -> Result<Self::Output, ServiceError> {
        let def = ctx.collection_def()?;

        // Trash needs soft delete — previously downgraded per surface.
        let trash = args.trash && def.soft_delete;

        let mut fq = args.query;

        // Filter hygiene for wire-decoded queries, identical on every surface:
        // dotted group paths (`seo.title`) normalize to their column form
        // (`seo__title`), and user filters must not touch system columns.
        // Previously gRPC/Lua did this in their codecs and MCP did neither —
        // so the same `where` clause parsed on two surfaces and 400'd on the
        // third.
        normalize_filter_fields(&mut fq.filters, &def.fields);
        validate_user_filters(&fq.filters).map_err(|e| ServiceError::HookError(e.to_string()))?;

        // Default sort for trash listings is newest-deleted-first. A
        // presentation default, but identical on every surface, so it lives
        // here rather than four times in the codecs.
        if trash && fq.order_by.is_none() {
            fq.order_by = Some(query::TRASH_DEFAULT_ORDER.to_string());
        }

        // Validate filter/order field names up front so a bad field is a
        // 400-class error on EVERY surface. Previously only gRPC pre-checked;
        // Lua/MCP/admin let the query runner's error surface as internal.
        query::validate_query_fields(def, &fq, args.locale_ctx.as_ref())
            .map_err(|e| ServiceError::HookError(e.to_string()))?;

        // `select` drives both the SQL column list (via the query) and the
        // post-process stripping (via the input) — derived from one source so
        // the two can't diverge.
        let select = fq.select.clone();

        let input = FindDocumentsInput::builder(&fq)
            .depth(args.depth)
            .hydrate(args.hydrate)
            .select(select.as_deref())
            .locale_ctx(args.locale_ctx.as_ref())
            .include_drafts(args.include_drafts)
            .status_filter(args.status_filter)
            .cursor_enabled(args.cursor_enabled)
            .trash(trash)
            .build();

        find_documents(ctx, &input)
    }
}
