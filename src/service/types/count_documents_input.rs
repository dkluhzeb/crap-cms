//! Input for `count_documents` — document counting with filters.

use crate::db::{FilterClause, LocaleContext};

/// Input for [`count_documents`](crate::service::count_documents).
pub struct CountDocumentsInput<'a> {
    pub filters: &'a [FilterClause],
    pub locale_ctx: Option<&'a LocaleContext>,
    pub search: Option<&'a str>,
    pub include_deleted: bool,
}

impl<'a> CountDocumentsInput<'a> {
    pub fn builder(filters: &'a [FilterClause]) -> CountDocumentsInputBuilder<'a> {
        CountDocumentsInputBuilder::new(filters)
    }
}

/// Builder for [`CountDocumentsInput`].
pub struct CountDocumentsInputBuilder<'a> {
    filters: &'a [FilterClause],
    locale_ctx: Option<&'a LocaleContext>,
    search: Option<&'a str>,
    include_deleted: bool,
}

impl<'a> CountDocumentsInputBuilder<'a> {
    pub fn new(filters: &'a [FilterClause]) -> Self {
        Self {
            filters,
            locale_ctx: None,
            search: None,
            include_deleted: false,
        }
    }

    pub fn locale_ctx(mut self, locale_ctx: Option<&'a LocaleContext>) -> Self {
        self.locale_ctx = locale_ctx;
        self
    }

    pub fn search(mut self, search: Option<&'a str>) -> Self {
        self.search = search;
        self
    }

    pub fn include_deleted(mut self, include_deleted: bool) -> Self {
        self.include_deleted = include_deleted;
        self
    }

    pub fn build(self) -> CountDocumentsInput<'a> {
        CountDocumentsInput {
            filters: self.filters,
            locale_ctx: self.locale_ctx,
            search: self.search,
            include_deleted: self.include_deleted,
        }
    }
}
