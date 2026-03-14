//! Builder for [`FindPaginationInput`].

use super::find::FindPaginationInput;

/// Builder for [`FindPaginationInput`].
pub(super) struct FindPaginationInputBuilder<'a> {
    find_query: &'a crate::db::FindQuery,
    docs: &'a [crate::core::Document],
    total: i64,
    pg_cursor: bool,
    pg_default: i64,
    lua_page: Option<i64>,
    has_timestamps: bool,
}

impl<'a> FindPaginationInputBuilder<'a> {
    pub(super) fn new(
        find_query: &'a crate::db::FindQuery,
        docs: &'a [crate::core::Document],
        total: i64,
    ) -> Self {
        Self {
            find_query,
            docs,
            total,
            pg_cursor: false,
            pg_default: 10,
            lua_page: None,
            has_timestamps: false,
        }
    }

    pub(super) fn pg_cursor(mut self, v: bool) -> Self {
        self.pg_cursor = v;
        self
    }

    pub(super) fn pg_default(mut self, v: i64) -> Self {
        self.pg_default = v;
        self
    }

    pub(super) fn lua_page(mut self, v: Option<i64>) -> Self {
        self.lua_page = v;
        self
    }

    pub(super) fn has_timestamps(mut self, v: bool) -> Self {
        self.has_timestamps = v;
        self
    }

    pub(super) fn build(self) -> FindPaginationInput<'a> {
        FindPaginationInput {
            find_query: self.find_query,
            docs: self.docs,
            total: self.total,
            pg_cursor: self.pg_cursor,
            pg_default: self.pg_default,
            lua_page: self.lua_page,
            has_timestamps: self.has_timestamps,
        }
    }
}
