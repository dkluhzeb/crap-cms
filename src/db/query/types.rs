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

/// A boolean filter expression over document fields.
///
/// A recursive tree: leaves are [`Filter`] conditions combined with `AND`/`OR`
/// to any depth. Composition is by construction — no normal-form expansion — so
/// callers nest freely and every evaluator (SQL generation, in-memory matching,
/// locale rewriting, validation) walks the tree with a single recursive pass.
///
/// Empty cases follow boolean algebra: an empty [`And`](FilterClause::And)
/// matches everything (`1=1`), an empty [`Or`](FilterClause::Or) matches nothing
/// (`1=0`).
#[derive(Debug, Clone)]
pub enum FilterClause {
    /// A single field condition.
    Single(Filter),
    /// Conjunction — every sub-clause must match.
    And(Vec<FilterClause>),
    /// Disjunction — at least one sub-clause must match.
    Or(Vec<FilterClause>),
}

impl FilterClause {
    /// Combine clauses with `AND`, collapsing a single clause to itself so the
    /// tree carries no redundant one-child nodes.
    #[must_use]
    pub fn and(mut clauses: Vec<FilterClause>) -> FilterClause {
        if clauses.len() == 1 {
            clauses.swap_remove(0)
        } else {
            FilterClause::And(clauses)
        }
    }

    /// Combine clauses with `OR`, collapsing a single clause to itself.
    #[must_use]
    pub fn or(mut clauses: Vec<FilterClause>) -> FilterClause {
        if clauses.len() == 1 {
            clauses.swap_remove(0)
        } else {
            FilterClause::Or(clauses)
        }
    }

    /// Build an OR-of-AND-groups from raw filter groups — the shape the query
    /// parsers (proto, admin URL filters, Lua) produce: `(g0…) OR (g1…)`. Each
    /// group's filters are AND-ed; the groups are OR-ed.
    #[must_use]
    pub fn or_groups(groups: Vec<Vec<Filter>>) -> FilterClause {
        FilterClause::or(
            groups
                .into_iter()
                .map(|g| FilterClause::and(g.into_iter().map(FilterClause::Single).collect()))
                .collect(),
        )
    }
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
    #[must_use]
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
    #[must_use]
    pub fn filters(mut self, filters: Vec<FilterClause>) -> Self {
        self.filters = filters;
        self
    }

    #[must_use]
    pub fn order_by(mut self, order_by: Option<String>) -> Self {
        self.order_by = order_by;
        self
    }

    #[must_use]
    pub fn limit(mut self, limit: Option<i64>) -> Self {
        self.limit = limit;
        self
    }

    #[must_use]
    pub fn offset(mut self, offset: Option<i64>) -> Self {
        self.offset = offset;
        self
    }

    #[must_use]
    pub fn select(mut self, select: Option<Vec<String>>) -> Self {
        self.select = select;
        self
    }

    #[must_use]
    pub fn after_cursor(mut self, cursor: Option<cursor::CursorData>) -> Self {
        self.after_cursor = cursor;
        self
    }

    #[must_use]
    pub fn before_cursor(mut self, cursor: Option<cursor::CursorData>) -> Self {
        self.before_cursor = cursor;
        self
    }

    #[must_use]
    pub fn search(mut self, search: Option<String>) -> Self {
        self.search = search;
        self
    }

    #[must_use]
    pub fn include_deleted(mut self, include: bool) -> Self {
        self.include_deleted = include;
        self
    }

    #[must_use]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_query_default_is_entirely_empty() {
        let q = FindQuery::default();
        assert!(q.filters.is_empty());
        assert!(q.order_by.is_none());
        assert!(q.limit.is_none());
        assert!(q.offset.is_none());
        assert!(q.select.is_none());
        assert!(q.after_cursor.is_none());
        assert!(q.before_cursor.is_none());
        assert!(q.search.is_none());
        assert!(!q.include_deleted);
    }

    /// Distinct values per field — notably `limit` ≠ `offset` — so a swapped
    /// or cross-wired assignment in `build()` surfaces as a mismatch.
    #[test]
    fn builder_wires_each_field_to_its_own_slot() {
        let q = FindQuery::builder()
            .filters(vec![FilterClause::Single(Filter {
                field: "status".into(),
                op: FilterOp::Equals("published".into()),
            })])
            .order_by(Some("-created_at".into()))
            .limit(Some(10))
            .offset(Some(20))
            .select(Some(vec!["title".into()]))
            .search(Some("hello".into()))
            .include_deleted(true)
            .build();

        assert_eq!(q.filters.len(), 1);
        assert_eq!(q.order_by.as_deref(), Some("-created_at"));
        assert_eq!(q.limit, Some(10));
        assert_eq!(q.offset, Some(20));
        assert_eq!(q.select, Some(vec!["title".to_string()]));
        assert_eq!(q.search.as_deref(), Some("hello"));
        assert!(q.include_deleted);
        assert!(q.after_cursor.is_none());
        assert!(q.before_cursor.is_none());
    }
}
