//! [`ViewScope`] — the resolved set of status views a read may see, and the
//! union filter it compiles to.
//!
//! The access model exposes content through independent status *views*:
//! published documents (gated by `access.read`) and draft documents (gated by
//! `access.draft`, which falls back to `update`). A read asks to see some set of
//! views; for each one the access hook yields an [`AccessResult`]. `ViewScope`
//! turns those per-view results into a single boolean [`FilterClause`]:
//!
//! ```text
//! (_status = 'published' AND <C_read>) OR (_status = 'draft' AND <C_draft>)
//! ```
//!
//! Denied views are dropped — reads downgrade, never error: asking for drafts
//! you cannot see yields published rows, not a 403. `_status` is injected here,
//! by the system; operators never write it in their own access filters.

use crate::db::query::types::{AccessResult, Filter, FilterClause, FilterOp};

/// The status views a read is requesting. Published is the base; draft is the
/// opt-in. Both together = the union (an admin "include drafts" listing).
#[derive(Debug, Clone, Copy)]
pub struct RequestedViews {
    pub published: bool,
    pub draft: bool,
}

impl RequestedViews {
    /// Construct from explicit flags.
    #[must_use]
    pub fn new(published: bool, draft: bool) -> Self {
        Self { published, draft }
    }

    /// A normal read: published content only.
    #[must_use]
    pub fn published_only() -> Self {
        Self::new(true, false)
    }

    /// A draft-inclusive read: the union of published and draft content.
    #[must_use]
    pub fn published_and_draft() -> Self {
        Self::new(true, true)
    }

    /// A draft-only read (admin `_status = draft` filter).
    #[must_use]
    pub fn draft_only() -> Self {
        Self::new(false, true)
    }
}

/// The resolved visibility for a read: which status views are visible and the
/// row filters constraining each. Built from per-view access results, compiled
/// to query filters via [`ViewScope::into_filters`].
#[derive(Debug, Clone)]
pub struct ViewScope {
    /// `Some(filters)` when published content is visible (empty = unconstrained);
    /// `None` when not requested or denied.
    published: Option<Vec<FilterClause>>,
    /// As `published`, for draft content.
    draft: Option<Vec<FilterClause>>,
    /// Whether the collection has a draft/`_status` axis at all. When false the
    /// scope never emits a `_status` filter — there is only published content.
    has_status_axis: bool,
}

impl ViewScope {
    /// Assemble a scope from the per-view access results. `None` = the view was
    /// not requested; `Some(Denied)` drops it (downgrade); `Some(Allowed)` makes
    /// it visible unconstrained; `Some(Constrained(c))` visible with filters `c`.
    #[must_use]
    pub fn assemble(
        has_status_axis: bool,
        published: Option<AccessResult>,
        draft: Option<AccessResult>,
    ) -> Self {
        Self {
            published: view_filters(published),
            draft: view_filters(draft),
            has_status_axis,
        }
    }

    /// Whether any status view is visible. When false the read sees nothing —
    /// the caller decides whether that is an empty result or an access error.
    #[must_use]
    pub fn is_anything_visible(&self) -> bool {
        self.published.is_some() || self.draft.is_some()
    }

    /// Whether published content is visible to this read.
    #[must_use]
    pub fn published_visible(&self) -> bool {
        self.published.is_some()
    }

    /// Whether draft content is visible to this read.
    #[must_use]
    pub fn draft_visible(&self) -> bool {
        self.draft.is_some()
    }

    /// The draft view's row constraints alone (without the `_status` guard) — the
    /// filter a single draft row must satisfy to be visible. Empty when the draft
    /// view is `Allowed` (unconstrained) or not visible. Used to bound a draft
    /// *snapshot* returned by id, which is filtered in memory rather than by SQL.
    #[must_use]
    pub fn draft_filters(&self) -> &[FilterClause] {
        self.draft.as_deref().unwrap_or(&[])
    }

    /// Compile the scope into the filter clauses to AND into the query.
    ///
    /// - No visible view → a single match-nothing clause (`Or([])` → `1=0`).
    /// - A status-axis collection → one `(_status = 'published' AND …) OR
    ///   (_status = 'draft' AND …)` clause over the visible branches.
    /// - A no-status-axis collection → just the read constraints (no `_status`);
    ///   fully-unconstrained visibility adds nothing.
    #[must_use]
    pub fn into_filters(self) -> Vec<FilterClause> {
        let ViewScope {
            published,
            draft,
            has_status_axis,
        } = self;

        let mut branches = Vec::new();
        if let Some(filters) = published {
            branches.push(branch(has_status_axis, "published", filters));
        }
        if let Some(filters) = draft {
            branches.push(branch(has_status_axis, "draft", filters));
        }

        if branches.is_empty() {
            return vec![match_nothing()];
        }

        match FilterClause::or(branches) {
            // An unconstrained single branch with no status axis is a no-op.
            FilterClause::And(parts) if parts.is_empty() => Vec::new(),
            clause => vec![clause],
        }
    }
}

/// Map one view's access result to its visibility + filters.
fn view_filters(result: Option<AccessResult>) -> Option<Vec<FilterClause>> {
    match result {
        None | Some(AccessResult::Denied) => None,
        Some(AccessResult::Allowed) => Some(Vec::new()),
        Some(AccessResult::Constrained(filters)) => Some(filters),
    }
}

/// Build one view's branch: the `_status` guard (when the collection has a
/// status axis) AND the view's row constraints.
fn branch(has_status_axis: bool, status: &str, filters: Vec<FilterClause>) -> FilterClause {
    let mut parts = Vec::with_capacity(filters.len() + 1);
    if has_status_axis {
        parts.push(status_is(status));
    }
    parts.extend(filters);
    FilterClause::and(parts)
}

fn status_is(status: &str) -> FilterClause {
    FilterClause::Single(Filter {
        field: "_status".to_string(),
        op: FilterOp::Equals(status.to_string()),
    })
}

/// A clause that matches no rows (`Or([])` renders as `1=0`).
fn match_nothing() -> FilterClause {
    FilterClause::Or(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pull the `_status` value out of a `Single(_status = …)` clause.
    fn status_of(clause: &FilterClause) -> Option<&str> {
        match clause {
            FilterClause::Single(Filter {
                field,
                op: FilterOp::Equals(v),
            }) if field == "_status" => Some(v.as_str()),
            _ => None,
        }
    }

    fn constrained(field: &str, value: &str) -> AccessResult {
        AccessResult::Constrained(vec![FilterClause::Single(Filter {
            field: field.to_string(),
            op: FilterOp::Equals(value.to_string()),
        })])
    }

    #[test]
    fn published_only_emits_published_status_guard() {
        let scope = ViewScope::assemble(true, Some(AccessResult::Allowed), None);
        assert!(scope.published_visible());
        assert!(!scope.draft_visible());

        let filters = scope.into_filters();
        assert_eq!(filters.len(), 1);
        assert_eq!(status_of(&filters[0]), Some("published"));
    }

    #[test]
    fn both_views_allowed_union_over_published_and_draft() {
        let scope = ViewScope::assemble(
            true,
            Some(AccessResult::Allowed),
            Some(AccessResult::Allowed),
        );
        let filters = scope.into_filters();
        assert_eq!(filters.len(), 1);

        let FilterClause::Or(branches) = &filters[0] else {
            panic!("expected an OR union, got {:?}", filters[0]);
        };
        assert_eq!(branches.len(), 2);
        assert_eq!(status_of(&branches[0]), Some("published"));
        assert_eq!(status_of(&branches[1]), Some("draft"));
    }

    #[test]
    fn draft_denied_downgrades_to_published_only() {
        let scope = ViewScope::assemble(
            true,
            Some(AccessResult::Allowed),
            Some(AccessResult::Denied),
        );
        assert!(scope.published_visible());
        assert!(!scope.draft_visible());

        let filters = scope.into_filters();
        // Single visible branch → no OR wrapper, just the published guard.
        assert_eq!(status_of(&filters[0]), Some("published"));
    }

    #[test]
    fn published_denied_draft_allowed_yields_draft_only() {
        let scope = ViewScope::assemble(
            true,
            Some(AccessResult::Denied),
            Some(AccessResult::Allowed),
        );
        assert!(!scope.published_visible());
        assert!(scope.draft_visible());

        let filters = scope.into_filters();
        assert_eq!(status_of(&filters[0]), Some("draft"));
    }

    #[test]
    fn constrained_view_ands_status_guard_with_row_filter() {
        // read = all published; draft = only my own (author = me).
        let scope = ViewScope::assemble(
            true,
            Some(AccessResult::Allowed),
            Some(constrained("author", "me")),
        );
        let filters = scope.into_filters();

        let FilterClause::Or(branches) = &filters[0] else {
            panic!("expected OR union");
        };
        // Published branch is a bare status guard.
        assert_eq!(status_of(&branches[0]), Some("published"));
        // Draft branch ANDs the status guard with the author constraint.
        let FilterClause::And(parts) = &branches[1] else {
            panic!(
                "expected AND for the constrained draft branch, got {:?}",
                branches[1]
            );
        };
        assert_eq!(parts.len(), 2);
        assert_eq!(status_of(&parts[0]), Some("draft"));
        assert!(matches!(
            &parts[1],
            FilterClause::Single(Filter { field, .. }) if field == "author"
        ));
    }

    #[test]
    fn nothing_visible_matches_no_rows() {
        let scope =
            ViewScope::assemble(true, Some(AccessResult::Denied), Some(AccessResult::Denied));
        assert!(!scope.is_anything_visible());

        let filters = scope.into_filters();
        // `Or([])` is the match-nothing sentinel (renders `1=0`).
        assert!(matches!(&filters[0], FilterClause::Or(v) if v.is_empty()));
    }

    #[test]
    fn no_status_axis_unconstrained_adds_no_filter() {
        // Collection without drafts: published read allowed, no `_status` column.
        let scope = ViewScope::assemble(false, Some(AccessResult::Allowed), None);
        assert!(
            scope.into_filters().is_empty(),
            "unconstrained read adds nothing"
        );
    }

    #[test]
    fn no_status_axis_constrained_emits_only_the_row_filter() {
        let scope = ViewScope::assemble(false, Some(constrained("author", "me")), None);
        let filters = scope.into_filters();
        assert_eq!(filters.len(), 1);
        // No `_status` guard — just the access constraint.
        assert!(matches!(
            &filters[0],
            FilterClause::Single(Filter { field, .. }) if field == "author"
        ));
    }

    #[test]
    fn not_requested_view_is_absent() {
        // draft = None means "not requested" — distinct from denied, but both
        // leave the view invisible.
        let scope = ViewScope::assemble(true, Some(AccessResult::Allowed), None);
        assert!(!scope.draft_visible());
    }
}
