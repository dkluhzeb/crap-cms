//! The `count` operation.

use crate::{
    db::{FilterClause, LocaleContext, query::filter::normalize_filter_fields},
    service::{
        CountDocumentsInput, ServiceContext, ServiceError, count_documents, validate_user_filters,
    },
};

use super::Operation;

/// Owned arguments for [`Count`]. Filters arrive already decoded into the
/// canonical [`FilterClause`] grammar — decoding from the wire shape (JSON
/// string / JSON object / Lua table / URL params) is the codec's job;
/// validation and system-filter injection are the service layer's.
pub struct CountArgs {
    pub filters: Vec<FilterClause>,
    pub locale_ctx: Option<LocaleContext>,
    pub search: Option<String>,
    /// Count drafts too (every surface passes this raw; the service scopes
    /// the count via the requested views).
    pub include_drafts: bool,
    /// Count the trash view instead of live rows.
    pub trash: bool,
}

impl CountArgs {
    #[must_use]
    pub fn builder(filters: Vec<FilterClause>) -> CountArgsBuilder {
        CountArgsBuilder::new(filters)
    }
}

/// Builder for [`CountArgs`].
pub struct CountArgsBuilder {
    filters: Vec<FilterClause>,
    locale_ctx: Option<LocaleContext>,
    search: Option<String>,
    include_drafts: bool,
    trash: bool,
}

impl CountArgsBuilder {
    fn new(filters: Vec<FilterClause>) -> Self {
        Self {
            filters,
            locale_ctx: None,
            search: None,
            include_drafts: false,
            trash: false,
        }
    }

    #[must_use]
    pub fn locale_ctx(mut self, locale_ctx: Option<LocaleContext>) -> Self {
        self.locale_ctx = locale_ctx;
        self
    }

    #[must_use]
    pub fn search(mut self, search: Option<String>) -> Self {
        self.search = search;
        self
    }

    #[must_use]
    pub fn include_drafts(mut self, include_drafts: bool) -> Self {
        self.include_drafts = include_drafts;
        self
    }

    #[must_use]
    pub fn trash(mut self, trash: bool) -> Self {
        self.trash = trash;
        self
    }

    #[must_use]
    pub fn build(self) -> CountArgs {
        CountArgs {
            filters: self.filters,
            locale_ctx: self.locale_ctx,
            search: self.search,
            include_drafts: self.include_drafts,
            trash: self.trash,
        }
    }
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
