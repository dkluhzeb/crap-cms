//! Query types: filters, find query, access result.

use crate::core::EventViewMeta;
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

/// Per-view read access for one collection/global, used to gate streamed
/// mutation events (gRPC Subscribe + admin SSE share this).
///
/// Each field holds `Some(constraints)` when that content view is visible to the
/// subscriber (empty = unconstrained, non-empty = row filter) or `None` when the
/// view is denied or its axis is absent for the slug. Built once at subscribe
/// time from the per-view access hooks, then consulted per event via
/// [`Self::constraints_for`] — the single place that maps an event's
/// [`EventViewMeta`] to the view that gates it, so the draft/trash leak cannot
/// be reintroduced by one surface drifting from the other.
#[derive(Debug, Default, Clone)]
pub struct EventViewGate {
    /// Published documents (`read`).
    pub published: Option<Vec<FilterClause>>,
    /// Draft documents (`draft`). `None` when the slug has no status axis.
    pub draft: Option<Vec<FilterClause>>,
    /// Trashed documents (`trash`). `None` when the slug has no soft-delete.
    pub trash: Option<Vec<FilterClause>>,
}

impl EventViewGate {
    /// Whether any content view is visible — the slug can be subscribed to at all.
    #[must_use]
    pub fn any_visible(&self) -> bool {
        self.published.is_some() || self.draft.is_some() || self.trash.is_some()
    }

    /// Select the row constraints that gate an event for its content view, or
    /// `None` when the subscriber cannot see that view (the event must be
    /// dropped). A trashed document is gated by `trash`; a draft by `draft`;
    /// anything else (published, or a status-less collection) by `read`. An
    /// empty slice means the view is visible without row constraints.
    #[must_use]
    pub fn constraints_for(&self, view: &EventViewMeta) -> Option<&[FilterClause]> {
        let selected = if view.trashed {
            &self.trash
        } else if view.status.as_deref() == Some("draft") {
            &self.draft
        } else {
            &self.published
        };

        selected.as_deref()
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

    fn name_filter(field: &str) -> FilterClause {
        FilterClause::Single(Filter {
            field: field.into(),
            op: FilterOp::Equals("x".into()),
        })
    }

    fn meta(status: Option<&str>, trashed: bool) -> EventViewMeta {
        EventViewMeta {
            status: status.map(str::to_string),
            trashed,
        }
    }

    #[test]
    fn event_view_gate_any_visible_reflects_each_axis() {
        assert!(!EventViewGate::default().any_visible());
        assert!(
            EventViewGate {
                published: Some(Vec::new()),
                ..Default::default()
            }
            .any_visible()
        );
        assert!(
            EventViewGate {
                draft: Some(Vec::new()),
                ..Default::default()
            }
            .any_visible()
        );
        assert!(
            EventViewGate {
                trash: Some(Vec::new()),
                ..Default::default()
            }
            .any_visible()
        );
    }

    #[test]
    fn constraints_for_routes_each_view_to_its_axis() {
        let gate = EventViewGate {
            published: Some(vec![name_filter("pub")]),
            draft: Some(vec![name_filter("draft")]),
            trash: Some(vec![name_filter("trash")]),
        };

        // Published / status-less → read view.
        let pub_c = gate
            .constraints_for(&meta(Some("published"), false))
            .unwrap();
        assert_eq!(pub_c.len(), 1);
        assert!(matches!(&pub_c[0], FilterClause::Single(f) if f.field == "pub"));
        let none_c = gate.constraints_for(&meta(None, false)).unwrap();
        assert!(matches!(&none_c[0], FilterClause::Single(f) if f.field == "pub"));

        // Draft status → draft view.
        let draft_c = gate.constraints_for(&meta(Some("draft"), false)).unwrap();
        assert!(matches!(&draft_c[0], FilterClause::Single(f) if f.field == "draft"));

        // Trashed → trash view, regardless of status.
        let trash_c = gate.constraints_for(&meta(Some("draft"), true)).unwrap();
        assert!(matches!(&trash_c[0], FilterClause::Single(f) if f.field == "trash"));
    }

    #[test]
    fn constraints_for_denied_view_returns_none() {
        // Only published visible: a draft event and a trash event are dropped,
        // a published event passes (unconstrained).
        let gate = EventViewGate {
            published: Some(Vec::new()),
            draft: None,
            trash: None,
        };
        assert!(
            gate.constraints_for(&meta(Some("published"), false))
                .is_some()
        );
        assert!(gate.constraints_for(&meta(Some("draft"), false)).is_none());
        assert!(gate.constraints_for(&meta(None, true)).is_none());
    }

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
