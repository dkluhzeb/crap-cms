//! Resolve the access-scoped read views — the published/draft status union
//! ([`resolve_view_scope`]) and the trash view ([`resolve_trash_scope`]).
//!
//! This is the single place a read decides which content a caller may see. The
//! live view calls `check_access` once per requested status view and hands the
//! per-view [`AccessResult`]s to [`ViewScope::assemble`], which builds the union
//! filter; draft is only consulted on a collection that actually has a status
//! axis (`has_drafts`). The trash view is a separate lifecycle mode gated by
//! `access.trash`. Both share [`ReadAccessCtx`].

use crate::core::{CollectionDefinition, Document, HookRef};
use crate::db::{AccessResult, Filter, FilterClause, FilterOp, RequestedViews, ViewScope};
use crate::hooks::AccessCheckInput;
use crate::service::ServiceError;
use crate::service::hooks::ReadHooks;
use crate::service::read::{validate_access_constraint_locales, validate_access_constraints};

/// Read context shared across a read's access checks — the subset of
/// [`AccessCheckInput`] identical for every view of one read.
pub(crate) struct ReadAccessCtx<'a> {
    pub def: &'a CollectionDefinition,
    pub slug: &'a str,
    pub user: Option<&'a Document>,
    pub id: Option<&'a str>,
    pub locale: Option<&'a str>,
    pub operation: &'a str,
    pub ui_locale: Option<&'a str>,
}

/// Resolve the visible status views for a live read by checking each requested
/// view's access hook.
///
/// # Errors
///
/// Returns a [`ServiceError`] if an access hook itself raises (e.g. a Lua
/// runtime error). Denied access is not an error — it downgrades the view away.
pub(crate) fn resolve_view_scope(
    hooks: &dyn ReadHooks,
    ctx: &ReadAccessCtx<'_>,
    requested: RequestedViews,
) -> Result<ViewScope, ServiceError> {
    let has_status_axis = ctx.def.has_drafts();

    let published = if requested.published {
        Some(check_view(hooks, ctx, ctx.def.access.read.as_ref())?)
    } else {
        None
    };

    // Draft content only exists on a collection with a status axis; otherwise a
    // draft request is moot and never reaches the access hook.
    let draft = if requested.draft && has_status_axis {
        Some(check_view(hooks, ctx, ctx.def.access.resolve_draft())?)
    } else {
        None
    };

    Ok(ViewScope::assemble(has_status_axis, published, draft))
}

/// Resolve the trash view's row constraints, gated by `access.trash` (falling
/// back to `update`). Independent of the published/draft status axis.
///
/// Returns the constraints to AND into the query, or an access-denied error.
///
/// # Errors
///
/// Returns [`ServiceError::AccessDenied`] when trash access is denied, or a
/// hook/validation error.
pub(crate) fn resolve_trash_scope(
    hooks: &dyn ReadHooks,
    ctx: &ReadAccessCtx<'_>,
) -> Result<Vec<FilterClause>, ServiceError> {
    resolve_trash_constraints(hooks, ctx)?
        .ok_or_else(|| ServiceError::AccessDenied("Trash access denied".into()))
}

/// Resolve the `access.trash` row constraints for the viewer (falling back to
/// `update`). The single place the trash gate is consulted — both the trash
/// *mode* read ([`resolve_trash_scope`]) and the cross-axis visibility union
/// ([`trash_branch`]) route through it so they can never drift apart.
///
/// - `Ok(None)` — trash access is denied.
/// - `Ok(Some(extra))` — allowed; `extra` are the row constraints to AND in (an
///   empty vec means unconstrained). The `_deleted_at` lifecycle guard is the
///   caller's job — this decides *whether* trashed rows are visible, not which
///   lifecycle state they're in. `check_view` already validated the constraints.
fn resolve_trash_constraints(
    hooks: &dyn ReadHooks,
    ctx: &ReadAccessCtx<'_>,
) -> Result<Option<Vec<FilterClause>>, ServiceError> {
    match check_view(hooks, ctx, ctx.def.access.resolve_trash())? {
        AccessResult::Denied => Ok(None),
        AccessResult::Allowed => Ok(Some(Vec::new())),
        AccessResult::Constrained(extra) => Ok(Some(extra)),
    }
}

/// Resolve the full cross-axis visibility of a collection for the viewer — the
/// rows of `ctx.def` they may see across BOTH the live status union (published
/// via `read`, draft via `draft`) AND the trash view (`access.trash`).
///
/// Unlike a normal read — which picks a single mode (live *or* trash) — some
/// surfaces must consider every view at once. Back-references are the canonical
/// case: a referrer should appear if the viewer could read it through *any*
/// view, and be hidden (and counted as inaccessible) otherwise.
///
/// Returns the filter clauses to intersect candidate rows with:
/// - `None` — nothing is visible; the viewer sees no rows of this collection.
/// - `Some(filters)` — visible rows match `filters`; an empty vec means every
///   row is visible (no status/lifecycle/access narrowing applies).
///
/// # Errors
///
/// Returns a [`ServiceError`] if an access hook raises or returns an invalid
/// row constraint.
pub(crate) fn resolve_visibility_filter(
    hooks: &dyn ReadHooks,
    ctx: &ReadAccessCtx<'_>,
) -> Result<Option<Vec<FilterClause>>, ServiceError> {
    let mut branches = Vec::new();

    // Live (non-trashed) status union: published ∪ draft.
    let scope = resolve_view_scope(hooks, ctx, RequestedViews::published_and_draft())?;
    if scope.is_anything_visible() {
        let mut parts = Vec::new();
        if ctx.def.soft_delete {
            parts.push(deleted_at(FilterOp::NotExists));
        }
        parts.extend(scope.into_filters());
        branches.push(FilterClause::and(parts));
    }

    // Trash view: soft-deleted rows the viewer may see via `access.trash`.
    if ctx.def.soft_delete
        && let Some(trash) = trash_branch(hooks, ctx)?
    {
        branches.push(trash);
    }

    if branches.is_empty() {
        return Ok(None);
    }

    match FilterClause::or(branches) {
        // A single unconstrained branch (no status axis, no soft-delete, read
        // allowed) collapses to match-all → no narrowing needed.
        FilterClause::And(parts) if parts.is_empty() => Ok(Some(Vec::new())),
        clause => Ok(Some(vec![clause])),
    }
}

/// Build the trash branch — `_deleted_at IS NOT NULL` AND any `access.trash`
/// row constraints — or `None` when trash access is denied.
fn trash_branch(
    hooks: &dyn ReadHooks,
    ctx: &ReadAccessCtx<'_>,
) -> Result<Option<FilterClause>, ServiceError> {
    let Some(constraints) = resolve_trash_constraints(hooks, ctx)? else {
        return Ok(None);
    };

    let mut parts = Vec::with_capacity(constraints.len() + 1);
    parts.push(deleted_at(FilterOp::Exists));
    parts.extend(constraints);

    Ok(Some(FilterClause::and(parts)))
}

/// A `_deleted_at` lifecycle guard: `Exists` = trashed, `NotExists` = live.
fn deleted_at(op: FilterOp) -> FilterClause {
    FilterClause::Single(Filter {
        field: "_deleted_at".to_string(),
        op,
    })
}

/// Run one view's access hook with the shared read context, validating any
/// returned row constraints.
fn check_view(
    hooks: &dyn ReadHooks,
    ctx: &ReadAccessCtx<'_>,
    access: Option<&HookRef>,
) -> Result<AccessResult, ServiceError> {
    let result = hooks.check_access(&AccessCheckInput {
        access,
        user: ctx.user,
        id: ctx.id,
        data: None,
        locale: ctx.locale,
        operation: ctx.operation,
        collection: ctx.slug,
        ui_locale: ctx.ui_locale,
    })?;

    // A view's row constraints may never touch system columns (the system
    // composes `_status`) or a locale-scoped field (its SQL vs in-memory value
    // spaces diverge). Validate here — the read-scope boundary every read
    // surface (runner and Lua CRUD) funnels through — so the reject is uniform.
    // The read scope never lets a view rule touch a system column: `_status` is
    // composed by the system (each key scopes its own status) and `_deleted_at`
    // is added by the trash branch — so both flags are `false` here.
    if let AccessResult::Constrained(filters) = &result {
        validate_access_constraints(
            filters, /* trash */ false, /* injecting_status */ false, ctx.slug,
        )?;
        validate_access_constraint_locales(filters, &ctx.def.fields, ctx.slug)?;
    }

    Ok(result)
}

/// Map a read's typed flags to the status views it requests. An explicit
/// `status_filter` (admin `?where[_status]=…`) names exact statuses; otherwise
/// `include_drafts` widens the default published read to the published+draft
/// union.
pub(crate) fn requested_views(
    status_filter: Option<&[String]>,
    include_drafts: bool,
) -> RequestedViews {
    match status_filter {
        // The model has exactly two status views: published and draft. Any value
        // that isn't "published" requests the draft view (admin validates the
        // status value upstream, so a bogus status never reaches here as a third
        // view — and even if it did, the draft view is still gated by `draft`).
        Some(values) if !values.is_empty() => RequestedViews::new(
            values.iter().any(|v| v == "published"),
            values.iter().any(|v| v != "published"),
        ),
        _ => RequestedViews::new(true, include_drafts),
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use std::cell::RefCell;
    use std::collections::HashMap;

    use anyhow::Result;

    use super::*;
    use crate::core::collection::{Access, VersionsConfig};
    use crate::core::{FieldDefinition, FieldDenial, collection::Hooks};
    use crate::db::{Filter, FilterClause, FilterOp};
    use crate::hooks::lifecycle::AfterReadCtx;

    /// Records which access refs were checked and replays canned results keyed
    /// by the ref string. Only `check_access` is exercised by `resolve_view_scope`.
    struct MockHooks {
        responses: HashMap<String, AccessResult>,
        calls: RefCell<Vec<String>>,
    }

    impl MockHooks {
        fn new(responses: &[(&str, AccessResult)]) -> Self {
            Self {
                responses: responses
                    .iter()
                    .map(|(k, v)| ((*k).to_string(), v.clone()))
                    .collect(),
                calls: RefCell::new(Vec::new()),
            }
        }
    }

    impl ReadHooks for MockHooks {
        fn before_read(&self, _: &Hooks, _: &str, _: &str, _: Option<&str>) -> Result<()> {
            Ok(())
        }

        fn after_read_one(&self, _: &AfterReadCtx, doc: Document) -> Document {
            doc
        }

        fn check_access(&self, input: &AccessCheckInput<'_>) -> Result<AccessResult> {
            let key = input
                .access
                .map(|r| r.reference().to_string())
                .unwrap_or_default();
            self.calls.borrow_mut().push(key.clone());

            Ok(self
                .responses
                .get(&key)
                .cloned()
                .unwrap_or(AccessResult::Denied))
        }

        fn field_read_denied(
            &self,
            _: &[FieldDefinition],
            _: Option<&Document>,
            _: Option<&str>,
        ) -> Vec<FieldDenial> {
            Vec::new()
        }
    }

    fn drafts_def() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("posts");
        def.versions = Some(VersionsConfig::new(true, 0));
        def.access = Access::builder()
            .read(Some(HookRef::new("read_fn")))
            .draft(Some(HookRef::new("draft_fn")))
            .build();
        def
    }

    fn ctx(def: &CollectionDefinition) -> ReadAccessCtx<'_> {
        ReadAccessCtx {
            def,
            slug: "posts",
            user: None,
            id: None,
            locale: None,
            operation: "find",
            ui_locale: None,
        }
    }

    #[test]
    fn published_only_checks_only_read() {
        let def = drafts_def();
        let hooks = MockHooks::new(&[("read_fn", AccessResult::Allowed)]);

        let scope =
            resolve_view_scope(&hooks, &ctx(&def), RequestedViews::published_only()).unwrap();

        assert_eq!(*hooks.calls.borrow(), vec!["read_fn".to_string()]);
        assert!(scope.published_visible());
        assert!(!scope.draft_visible());
    }

    #[test]
    fn with_drafts_checks_read_then_draft() {
        let def = drafts_def();
        let hooks = MockHooks::new(&[
            ("read_fn", AccessResult::Allowed),
            ("draft_fn", AccessResult::Allowed),
        ]);

        let scope =
            resolve_view_scope(&hooks, &ctx(&def), RequestedViews::published_and_draft()).unwrap();

        assert_eq!(
            *hooks.calls.borrow(),
            vec!["read_fn".to_string(), "draft_fn".to_string()]
        );
        assert!(scope.published_visible());
        assert!(scope.draft_visible());
    }

    /// A collection without a status axis must never consult the draft hook,
    /// even when drafts are requested — there is no draft content to gate.
    #[test]
    fn draft_request_suppressed_on_non_draft_collection() {
        let mut def = CollectionDefinition::new("pages");
        def.access = Access::builder()
            .read(Some(HookRef::new("read_fn")))
            .draft(Some(HookRef::new("draft_fn")))
            .build();
        // No versions config → has_drafts() is false.
        let hooks = MockHooks::new(&[("read_fn", AccessResult::Allowed)]);

        let scope =
            resolve_view_scope(&hooks, &ctx(&def), RequestedViews::published_and_draft()).unwrap();

        assert_eq!(
            *hooks.calls.borrow(),
            vec!["read_fn".to_string()],
            "draft hook must not be called on a non-draft collection"
        );
        assert!(!scope.draft_visible());
    }

    #[test]
    fn draft_denied_downgrades_to_published() {
        let def = drafts_def();
        let hooks = MockHooks::new(&[
            ("read_fn", AccessResult::Allowed),
            ("draft_fn", AccessResult::Denied),
        ]);

        let scope =
            resolve_view_scope(&hooks, &ctx(&def), RequestedViews::published_and_draft()).unwrap();

        assert!(scope.published_visible());
        assert!(!scope.draft_visible());
    }

    /// A view's access hook may not constrain on a system column — the system
    /// owns `_status`. Such a constraint is rejected, not silently honored.
    #[test]
    fn rejects_constrained_filter_on_system_column() {
        let def = drafts_def();
        let hooks = MockHooks::new(&[(
            "read_fn",
            AccessResult::Constrained(vec![FilterClause::Single(Filter {
                field: "_status".to_string(),
                op: FilterOp::Equals("published".to_string()),
            })]),
        )]);

        let err =
            resolve_view_scope(&hooks, &ctx(&def), RequestedViews::published_only()).unwrap_err();
        assert!(matches!(err, ServiceError::HookError(_)));
    }

    #[test]
    fn requested_views_maps_flags() {
        let pub_only = requested_views(None, false);
        assert!(pub_only.published && !pub_only.draft);

        let with_drafts = requested_views(None, true);
        assert!(with_drafts.published && with_drafts.draft);

        let draft = ["draft".to_string()];
        let draft_only = requested_views(Some(&draft), false);
        assert!(!draft_only.published && draft_only.draft);

        let both = ["published".to_string(), "draft".to_string()];
        let both_views = requested_views(Some(&both), false);
        assert!(both_views.published && both_views.draft);
    }

    /// A draft+soft-delete collection with read allowed but draft/trash denied
    /// exposes only live published rows.
    #[test]
    fn visibility_published_only_when_draft_and_trash_denied() {
        let mut def = drafts_def();
        def.soft_delete = true;
        def.access.trash = Some(HookRef::new("trash_fn"));
        let hooks = MockHooks::new(&[("read_fn", AccessResult::Allowed)]);

        let filters = resolve_visibility_filter(&hooks, &ctx(&def))
            .unwrap()
            .expect("published is visible");

        // Single live branch: _deleted_at IS NULL AND _status = 'published'.
        let FilterClause::And(parts) = &filters[0] else {
            panic!("expected a live AND branch, got {:?}", filters[0]);
        };
        assert!(parts.iter().any(|c| matches!(
            c,
            FilterClause::Single(Filter { field, op: FilterOp::NotExists }) if field == "_deleted_at"
        )));
        assert!(parts.iter().any(|c| matches!(
            c,
            FilterClause::Single(Filter { field, op: FilterOp::Equals(v) })
                if field == "_status" && v == "published"
        )));
    }

    /// When every view is denied, nothing is visible — the owner is dropped.
    #[test]
    fn visibility_nothing_visible_is_none() {
        let mut def = drafts_def();
        def.soft_delete = true;
        def.access.trash = Some(HookRef::new("trash_fn"));
        let hooks = MockHooks::new(&[]); // all default to Denied

        assert!(
            resolve_visibility_filter(&hooks, &ctx(&def))
                .unwrap()
                .is_none()
        );
    }

    /// Trash access adds a soft-deleted branch alongside the live union.
    #[test]
    fn visibility_unions_live_and_trash() {
        let mut def = drafts_def();
        def.soft_delete = true;
        def.access.trash = Some(HookRef::new("trash_fn"));
        let hooks = MockHooks::new(&[
            ("read_fn", AccessResult::Allowed),
            ("trash_fn", AccessResult::Allowed),
        ]);

        let filters = resolve_visibility_filter(&hooks, &ctx(&def))
            .unwrap()
            .expect("live + trash visible");

        let FilterClause::Or(branches) = &filters[0] else {
            panic!("expected an OR of live + trash, got {:?}", filters[0]);
        };
        assert_eq!(branches.len(), 2, "one live branch, one trash branch");

        // The trash branch carries _deleted_at IS NOT NULL (Exists). With no
        // trash row-constraints it collapses to that single guard clause, so
        // flatten each branch to its parts before scanning.
        let has_trash = branches.iter().any(|b| {
            let parts: &[FilterClause] = match b {
                FilterClause::And(parts) => parts,
                other => std::slice::from_ref(other),
            };
            parts.iter().any(|c| matches!(
                c,
                FilterClause::Single(Filter { field, op: FilterOp::Exists }) if field == "_deleted_at"
            ))
        });
        assert!(has_trash, "trash branch must guard _deleted_at IS NOT NULL");
    }

    /// A collection with no status axis and no soft-delete, read allowed
    /// unconstrained, imposes no narrowing — every row is visible.
    #[test]
    fn visibility_unconstrained_no_axis_is_match_all() {
        let mut def = CollectionDefinition::new("pages");
        def.access = Access::builder()
            .read(Some(HookRef::new("read_fn")))
            .build();
        let hooks = MockHooks::new(&[("read_fn", AccessResult::Allowed)]);

        let filters = resolve_visibility_filter(&hooks, &ctx(&def))
            .unwrap()
            .expect("read is allowed");
        assert!(filters.is_empty(), "match-all imposes no filter");
    }

    #[test]
    fn trash_scope_denied_is_access_error() {
        let mut def = drafts_def();
        def.access.trash = Some(HookRef::new("trash_fn"));
        let hooks = MockHooks::new(&[("trash_fn", AccessResult::Denied)]);

        let err = resolve_trash_scope(&hooks, &ctx(&def)).unwrap_err();
        assert!(matches!(err, ServiceError::AccessDenied(_)));
    }

    #[test]
    fn trash_scope_allowed_has_no_constraints() {
        let mut def = drafts_def();
        def.access.trash = Some(HookRef::new("trash_fn"));
        let hooks = MockHooks::new(&[("trash_fn", AccessResult::Allowed)]);

        let filters = resolve_trash_scope(&hooks, &ctx(&def)).unwrap();
        assert!(filters.is_empty());
    }

    #[test]
    fn trash_scope_falls_back_to_update_gate() {
        // No `trash` rule → `resolve_trash` falls back to `update`.
        let mut def = drafts_def();
        def.access.update = Some(HookRef::new("update_fn"));
        let hooks = MockHooks::new(&[("update_fn", AccessResult::Allowed)]);

        let filters = resolve_trash_scope(&hooks, &ctx(&def)).unwrap();
        assert!(filters.is_empty());
        assert_eq!(*hooks.calls.borrow(), vec!["update_fn".to_string()]);
    }
}
