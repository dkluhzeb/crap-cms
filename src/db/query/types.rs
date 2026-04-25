//! Query types: filters, find query, access result.

use crate::db::query::cursor;

/// Result of an access control check.
#[derive(Debug, Clone)]
pub enum AccessResult {
    /// Access allowed, no restrictions.
    Allowed,
    /// Access denied.
    Denied,
    /// Access allowed with constraints (read only). Additional query filters to merge.
    Constrained(Vec<FilterClause>),
}

/// A filter comparison operator with its operand value(s).
#[derive(Debug, Clone)]
pub enum FilterOp {
    Equals(String),
    NotEquals(String),
    Like(String),
    Contains(String),
    GreaterThan(String),
    LessThan(String),
    GreaterThanOrEqual(String),
    LessThanOrEqual(String),
    In(Vec<String>),
    NotIn(Vec<String>),
    Exists,
    NotExists,
}

/// A single field + operator filter condition.
#[derive(Debug, Clone)]
pub struct Filter {
    pub field: String,
    pub op: FilterOp,
}

/// A filter clause: either a single condition or an OR group.
/// Each OR element is a group of AND-ed filters: `(a AND b) OR (c AND d)`.
#[derive(Debug, Clone)]
pub enum FilterClause {
    Single(Filter),
    Or(Vec<Vec<Filter>>),
}

/// Parameters for a find query: filters, ordering, pagination, and field selection.
#[derive(Debug, Default, Clone)]
pub struct FindQuery {
    pub filters: Vec<FilterClause>,
    pub order_by: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
    /// Optional list of fields to return. `None` = all fields.
    /// Always includes `id`, `created_at`, `updated_at`.
    pub select: Option<Vec<String>>,
    /// Forward cursor for keyset pagination. Mutually exclusive with `offset` and `before_cursor`.
    pub after_cursor: Option<cursor::CursorData>,
    /// Backward cursor for keyset pagination. Mutually exclusive with `offset` and `after_cursor`.
    pub before_cursor: Option<cursor::CursorData>,
    /// FTS5 full-text search query. When set, results are filtered to documents
    /// matching this search term via the FTS5 index.
    pub search: Option<String>,
    /// When true, include soft-deleted documents in results.
    /// Default false — soft-deleted docs are excluded from normal queries.
    pub include_deleted: bool,
}

impl FindQuery {
    /// Create a builder for constructing a `FindQuery` with named parameters.
    ///
    /// This is the only public construction path — `FindQuery` derives
    /// `Default`, so tests that need an "empty" query use `FindQuery::default()`
    /// or struct literals with `..Default::default()`. Production code must
    /// route through the builder.
    pub fn builder() -> FindQueryBuilder {
        FindQueryBuilder::default()
    }
}

/// Builder for [`FindQuery`]. Created via [`FindQuery::builder()`].
#[derive(Default)]
pub struct FindQueryBuilder {
    filters: Vec<FilterClause>,
    order_by: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
    select: Option<Vec<String>>,
    after_cursor: Option<cursor::CursorData>,
    before_cursor: Option<cursor::CursorData>,
    search: Option<String>,
    include_deleted: bool,
}

impl FindQueryBuilder {
    pub fn filters(mut self, filters: Vec<FilterClause>) -> Self {
        self.filters = filters;
        self
    }

    pub fn order_by(mut self, order_by: Option<String>) -> Self {
        self.order_by = order_by;
        self
    }

    pub fn limit(mut self, limit: Option<i64>) -> Self {
        self.limit = limit;
        self
    }

    pub fn offset(mut self, offset: Option<i64>) -> Self {
        self.offset = offset;
        self
    }

    pub fn select(mut self, select: Option<Vec<String>>) -> Self {
        self.select = select;
        self
    }

    pub fn after_cursor(mut self, cursor: Option<cursor::CursorData>) -> Self {
        self.after_cursor = cursor;
        self
    }

    pub fn before_cursor(mut self, cursor: Option<cursor::CursorData>) -> Self {
        self.before_cursor = cursor;
        self
    }

    pub fn search(mut self, search: Option<String>) -> Self {
        self.search = search;
        self
    }

    pub fn include_deleted(mut self, include: bool) -> Self {
        self.include_deleted = include;
        self
    }

    pub fn build(self) -> FindQuery {
        FindQuery {
            filters: self.filters,
            order_by: self.order_by,
            limit: self.limit,
            offset: self.offset,
            select: self.select,
            after_cursor: self.after_cursor,
            before_cursor: self.before_cursor,
            search: self.search,
            include_deleted: self.include_deleted,
        }
    }
}
