//! The `access.versions` restriction toggle for version history.

use crate::{
    db::AccessResult,
    hooks::AccessCheckInput,
    service::{Def, ReadHooks, ServiceContext, ServiceError, helpers::enforce_access_constraints},
};

/// Enforce the `access.versions` gate before listing or fetching version
/// history.
///
/// An explicit `access.versions` is a pure *toggle*: it must return
/// `true`/`false` (a filter table is a configuration error — row-level scoping
/// belongs on `read`), and it can only further restrict.
///
/// When `access.versions` is **unset**, version access falls back to edit access
/// (`access.update`) — mirroring how `draft`/`trash` fall back to `update`. So a
/// published-only reader sees no history by default, an editor does, and enabling
/// the `versions` feature works out of the box without a separate access rule. A
/// row-filtered `update` (e.g. `{ author = me }`) scopes *which* documents'
/// history is visible. Either way, per-snapshot *content* is independently
/// bounded by the `read`/`draft` composite at the calling surface.
///
/// # Errors
///
/// Returns [`ServiceError::AccessDenied`] when access denies history, or
/// [`ServiceError::HookError`] when an explicit `versions` returns a filter table
/// (or a hook raises).
pub(super) fn check_versions_gate(
    ctx: &ServiceContext,
    hooks: &dyn ReadHooks,
    id: Option<&str>,
    operation: &str,
) -> Result<(), ServiceError> {
    // An explicit toggle takes precedence and is boolean-only.
    if let Some(versions_ref) = ctx.versions_access_ref() {
        let access = hooks.check_access(&AccessCheckInput {
            access: Some(versions_ref),
            user: ctx.user,
            id,
            data: None,
            locale: None,
            operation,
            collection: ctx.slug,
            ui_locale: None,
        })?;

        return versions_gate_decision(&access, ctx.slug);
    }

    // Unset → fall back to `access.update` (history follows edit access).
    let access = hooks.check_access(&AccessCheckInput {
        access: ctx.update_access_ref(),
        user: ctx.user,
        id,
        data: None,
        locale: None,
        operation,
        collection: ctx.slug,
        ui_locale: None,
    })?;

    versions_via_update_decision(ctx, &access, id, operation)
}

/// Interpret an `access.update` result used as the version-history fallback.
/// Unlike the explicit toggle, a `Constrained` result is a legitimate
/// row-ownership scope (not a config error): the timeline is visible for
/// documents the caller may edit. It is enforced against the target document
/// when the surface knows it (`list_versions`); for `find_version_by_id` (no id
/// here) the surface independently gates snapshot content by `read`, so the
/// timeline is allowed and disclosure is bounded there.
fn versions_via_update_decision(
    ctx: &ServiceContext,
    access: &AccessResult,
    id: Option<&str>,
    operation: &str,
) -> Result<(), ServiceError> {
    let denied = || ServiceError::AccessDenied("Version history access denied".into());

    match access {
        AccessResult::Allowed => Ok(()),
        AccessResult::Denied => Err(denied()),
        AccessResult::Constrained(_) => match &ctx.def {
            Def::Global(_) => Err(reject_global_filter(ctx.slug)),
            _ => match id {
                Some(doc_id) => {
                    match enforce_access_constraints(ctx, doc_id, access, operation, false) {
                        Ok(()) => Ok(()),
                        Err(ServiceError::AccessDenied(_)) => Err(denied()),
                        Err(e) => Err(e),
                    }
                }
                None => Ok(()),
            },
        },
    }
}

/// Interpret an `access.versions` toggle result. Shared by the read-side gate
/// ([`check_versions_gate`]) and the write-side restore gate so both agree:
/// `Allowed` → proceed, `Denied` → access error, `Constrained` → config error
/// (the toggle is boolean; row scoping belongs on `read`).
pub(crate) fn versions_gate_decision(
    access: &AccessResult,
    slug: &str,
) -> Result<(), ServiceError> {
    match access {
        AccessResult::Allowed => Ok(()),
        AccessResult::Denied => Err(ServiceError::AccessDenied(
            "Version history access denied".into(),
        )),
        AccessResult::Constrained(_) => Err(ServiceError::HookError(format!(
            "Access hook 'access.versions' for '{slug}' returned a filter table; version access \
             is a toggle — return true/false based on ctx.user instead (row-level scoping is \
             handled by the read access rule)."
        ))),
    }
}

/// Resolve whether *draft* version snapshots are visible to the caller for a
/// given parent document. `read` has already passed; this consults the
/// edit-level `draft` access (`access.draft ?? access.update`).
///
/// `Denied` → not visible (published snapshots only). `Allowed` → visible. A
/// `Constrained` rule (e.g. "preview only your own drafts") is enforced against
/// the parent document: a non-match downgrades to *not visible* rather than
/// leaking another owner's draft snapshots — mirroring the `find_by_id` draft
/// gate. A filter table on a global is a configuration error.
///
/// # Errors
///
/// Returns [`ServiceError::HookError`] for a filter table on a global, or a
/// backend error if the constraint count query fails.
pub(super) fn draft_snapshots_visible(
    ctx: &ServiceContext,
    draft_access: &AccessResult,
    parent_id: &str,
) -> Result<bool, ServiceError> {
    match draft_access {
        AccessResult::Allowed => Ok(true),
        AccessResult::Denied => Ok(false),
        AccessResult::Constrained(_) => match &ctx.def {
            Def::Global(_) => Err(reject_global_filter(ctx.slug)),
            _ => match enforce_access_constraints(ctx, parent_id, draft_access, "Read", false) {
                Ok(()) => Ok(true),
                Err(ServiceError::AccessDenied(_)) => Ok(false),
                Err(e) => Err(e),
            },
        },
    }
}

/// The standard "globals don't support filter-based access" error, raised when
/// a global's `read`/`draft` access hook returns a filter table on a version
/// surface (a single-row global can't be row-scoped).
pub(super) fn reject_global_filter(slug: &str) -> ServiceError {
    ServiceError::HookError(format!(
        "Access hook for global '{slug}' returned a filter table; globals don't support \
         filter-based access — return true/false based on ctx.user fields instead."
    ))
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use anyhow::Result;

    use super::*;
    use crate::core::collection::Hooks;
    use crate::core::{CollectionDefinition, Document, FieldDefinition, FieldDenial, HookRef};
    use crate::hooks::lifecycle::AfterReadCtx;

    /// Returns a canned access result and records whether `check_access` ran
    /// (so a test can prove the unset toggle short-circuits without a hook call)
    /// plus the `operation` it was handed (so a test can prove the surface's
    /// real op is forwarded, not a hard-coded placeholder).
    struct GateHooks {
        result: AccessResult,
        called: Cell<bool>,
        last_operation: RefCell<Option<String>>,
    }

    impl GateHooks {
        fn new(result: AccessResult) -> Self {
            Self {
                result,
                called: Cell::new(false),
                last_operation: RefCell::new(None),
            }
        }
    }

    impl ReadHooks for GateHooks {
        fn before_read(&self, _: &Hooks, _: &str, _: &str, _: Option<&str>) -> Result<()> {
            Ok(())
        }

        fn after_read_one(&self, _: &AfterReadCtx, doc: Document) -> Document {
            doc
        }

        fn check_access(&self, input: &AccessCheckInput<'_>) -> Result<AccessResult> {
            self.called.set(true);
            *self.last_operation.borrow_mut() = Some(input.operation.to_string());
            Ok(self.result.clone())
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

    fn def_with_versions(versions: Option<HookRef>) -> CollectionDefinition {
        let mut def = CollectionDefinition::new("posts");
        def.access.versions = versions;
        def
    }

    /// Unset toggle → falls back to `access.update`: when update ALLOWS, history
    /// is allowed (an editor sees history out of the box).
    #[test]
    fn unset_versions_falls_back_to_update_allowed() {
        let def = def_with_versions(None);
        let ctx = ServiceContext::collection("posts", &def).build();
        let hooks = GateHooks::new(AccessResult::Allowed);

        assert!(check_versions_gate(&ctx, &hooks, None, "find").is_ok());
        assert!(
            hooks.called.get(),
            "an unset toggle must consult the update fallback"
        );
    }

    /// Unset toggle → falls back to `access.update`: when update DENIES (e.g. a
    /// published-only reader with no edit access), history is denied — the key
    /// change from the old blanket-allow default.
    #[test]
    fn unset_versions_falls_back_to_update_denied() {
        let def = def_with_versions(None);
        let ctx = ServiceContext::collection("posts", &def).build();
        let hooks = GateHooks::new(AccessResult::Denied);

        let err = check_versions_gate(&ctx, &hooks, None, "find").unwrap_err();
        assert!(
            matches!(err, ServiceError::AccessDenied(_)),
            "a reader without edit access must be denied version history"
        );
    }

    /// Unset toggle + a `Constrained` update is NOT a config error (unlike an
    /// explicit `versions` filter): it's a row-ownership scope. With no document
    /// id at the gate (`find_version_by_id`), the timeline is allowed and the
    /// surface's own `read` gate bounds snapshot content.
    #[test]
    fn unset_versions_constrained_update_without_id_allows() {
        let def = def_with_versions(None);
        let ctx = ServiceContext::collection("posts", &def).build();
        let hooks = GateHooks::new(AccessResult::Constrained(Vec::new()));

        assert!(
            check_versions_gate(&ctx, &hooks, None, "find_by_id").is_ok(),
            "a constrained update fallback with no id must allow (content gated by read)"
        );
    }

    #[test]
    fn versions_allowed_passes_and_forwards_operation() {
        let def = def_with_versions(Some(HookRef::new("versions_fn")));
        let ctx = ServiceContext::collection("posts", &def).build();
        let hooks = GateHooks::new(AccessResult::Allowed);

        assert!(check_versions_gate(&ctx, &hooks, Some("p1"), "find_by_id").is_ok());
        assert!(hooks.called.get(), "a set toggle must run the hook");
        assert_eq!(
            hooks.last_operation.borrow().as_deref(),
            Some("find_by_id"),
            "the surface's real operation must reach the hook, not a placeholder"
        );
    }

    #[test]
    fn versions_denied_is_access_error() {
        let def = def_with_versions(Some(HookRef::new("versions_fn")));
        let ctx = ServiceContext::collection("posts", &def).build();
        let hooks = GateHooks::new(AccessResult::Denied);

        let err = check_versions_gate(&ctx, &hooks, None, "find").unwrap_err();
        assert!(matches!(err, ServiceError::AccessDenied(_)));
    }

    /// A filter table on the toggle is a configuration error, not silently
    /// treated as allow — row scoping belongs on `read`.
    #[test]
    fn versions_constrained_is_hook_error() {
        let def = def_with_versions(Some(HookRef::new("versions_fn")));
        let ctx = ServiceContext::collection("posts", &def).build();
        let hooks = GateHooks::new(AccessResult::Constrained(Vec::new()));

        let err = check_versions_gate(&ctx, &hooks, None, "find").unwrap_err();
        assert!(matches!(err, ServiceError::HookError(_)));
    }
}
