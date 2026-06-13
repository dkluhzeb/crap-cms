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
use crate::db::{AccessResult, FilterClause, RequestedViews, ViewScope};
use crate::hooks::AccessCheckInput;
use crate::service::ServiceError;
use crate::service::hooks::ReadHooks;
use crate::service::read::validate_access_constraints;

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
    let access = check_view(hooks, ctx, ctx.def.access.resolve_trash())?;

    match access {
        AccessResult::Denied => Err(ServiceError::AccessDenied("Trash access denied".into())),
        AccessResult::Allowed => Ok(Vec::new()),
        // `check_view` validated with the live rule (no system columns); trash
        // constraints legitimately need none either — the system adds the
        // `_deleted_at` guard itself.
        AccessResult::Constrained(extra) => Ok(extra),
    }
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

    // A view's row constraints may never touch system columns: the system
    // composes `_status` itself (each view scopes its own status), so an
    // operator filtering `_status`/`_deleted_at` is always a mistake here.
    if let AccessResult::Constrained(filters) = &result {
        validate_access_constraints(filters, false, false, ctx.slug)?;
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
