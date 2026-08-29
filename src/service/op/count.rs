//! The `count` operation.

use crate::{
    db::{FilterClause, LocaleContext, query::filter::normalize_filter_fields},
    service::{
        CountDocumentsInput, ServiceContext, ServiceError, count_documents, validate_user_filters,
    },
};

use crate::core::Builder;

use super::Operation;

/// Owned arguments for [`Count`]. Filters arrive already decoded into the
/// canonical [`FilterClause`] grammar — decoding from the wire shape (JSON
/// string / JSON object / Lua table / URL params) is the codec's job;
/// validation and system-filter injection are the service layer's.
#[derive(Builder)]
pub struct CountArgs {
    #[builder(required)]
    pub filters: Vec<FilterClause>,
    pub locale_ctx: Option<LocaleContext>,
    pub search: Option<String>,
    /// Count drafts too (every surface passes this raw; the service scopes
    /// the count via the requested views).
    pub include_drafts: bool,
    /// Count the trash view instead of live rows.
    pub trash: bool,
}

/// Count documents matching filters (no per-document hooks).
pub enum Count {}

impl Operation for Count {
    type Args = CountArgs;
    type Output = i64;

    const NAME: &'static str = "count";

    fn run(ctx: &ServiceContext<'_>, args: Self::Args) -> Result<Self::Output, ServiceError> {
        let def = ctx.collection_def()?;

        // Same filter hygiene and trash downgrade as `Find::run`, so a count
        // always agrees with the list it summarizes — on every surface.
        let mut filters = args.filters;
        normalize_filter_fields(&mut filters, &def.fields);
        validate_user_filters(&filters).map_err(|e| ServiceError::HookError(e.to_string()))?;

        let trash = args.trash && def.soft_delete;

        let input = CountDocumentsInput::builder(&filters)
            .locale_ctx(args.locale_ctx.as_ref())
            .search(args.search.as_deref())
            .include_drafts(args.include_drafts)
            .trash(trash)
            .build();

        count_documents(ctx, &input)
    }
}
