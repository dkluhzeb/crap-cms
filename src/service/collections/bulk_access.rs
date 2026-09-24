//! Shared match-set scoping for bulk operations (`update_many` /
//! `delete_many`).
//!
//! Before this chokepoint existed, each surface carried its own (diverging)
//! pre-flight: gRPC gated bulk writes on `access.read` and added the
//! published-only filter itself, Lua gated on the operation's own access fn,
//! and MCP (override-access) had none. The service is now self-gating, so a
//! codec cannot drift.

use crate::{
    core::{CollectionDefinition, DocumentFields, HookRef},
    db::{AccessResult, Filter, FilterClause, FilterOp, LocaleContext},
    hooks::AccessCheckInput,
    service::{
        FieldReadStrip, ServiceContext, ServiceError, hooks::WriteHooks,
        reject_unreadable_filter_fields,
    },
};

/// What a bulk operation is gated by. Built with [`BulkScope::builder`]: the
/// update gate in `update_many`, the delete-family gate in [`delete_scope`].
pub(crate) struct BulkScope<'a> {
    /// Access operation string (`"update"`, `"trash"`, `"delete"`).
    pub operation: &'a str,
    /// The access hook to evaluate (the operation's own gate).
    pub access_fn: Option<&'a HookRef>,
    /// Incoming patch, exposed to the access fn as `ctx.data` (update only).
    pub data: Option<&'a DocumentFields>,
    /// Whether the caller injects `_status = published` itself (update with
    /// drafts, no draft opt-in) — the constraint validator then allows a
    /// `_status` constraint from the access hook.
    pub injecting_status: bool,
    /// The operation's locale, in which read-gated filter fields are judged
    /// (the default locale without one — as the write reports its result).
    pub locale_ctx: Option<&'a LocaleContext>,
}

impl<'a> BulkScope<'a> {
    /// Start a scope gated by the access `operation`: no access hook, no
    /// patch, no injected `_status`, the default locale.
    #[must_use]
    pub(crate) fn builder(operation: &'a str) -> BulkScopeBuilder<'a> {
        BulkScopeBuilder {
            scope: BulkScope {
                operation,
                access_fn: None,
                data: None,
                injecting_status: false,
                locale_ctx: None,
            },
        }
    }
}

/// Builder for [`BulkScope`].
pub(crate) struct BulkScopeBuilder<'a> {
    scope: BulkScope<'a>,
}

impl<'a> BulkScopeBuilder<'a> {
    #[must_use]
    pub(crate) fn access_fn(mut self, access_fn: Option<&'a HookRef>) -> Self {
        self.scope.access_fn = access_fn;
        self
    }

    #[must_use]
    pub(crate) fn data(mut self, data: Option<&'a DocumentFields>) -> Self {
        self.scope.data = data;
        self
    }

    #[must_use]
    pub(crate) fn injecting_status(mut self, injecting_status: bool) -> Self {
        self.scope.injecting_status = injecting_status;
        self
    }

    #[must_use]
    pub(crate) fn locale_ctx(mut self, locale_ctx: Option<&'a LocaleContext>) -> Self {
        self.scope.locale_ctx = locale_ctx;
        self
    }

    #[must_use]
    pub(crate) fn build(self) -> BulkScope<'a> {
        self.scope
    }
}

/// Gate the bulk operation and scope its match-set: reject a filter on a field
/// the caller may not read, then run the access hook once up front — `Denied`
/// errors before anything is matched, `Constrained` appends the row filters so
/// out-of-scope rows are never matched. Skipped under `override_access`,
/// matching the single-op paths. The per-document lifecycle still enforces
/// per-doc access; this bounds WHICH rows are matched and provides the early
/// denial.
pub(super) fn scope_bulk_access(
    ctx: &ServiceContext,
    hooks: &dyn WriteHooks,
    scope: &BulkScope<'_>,
    filters: &mut Vec<FilterClause>,
) -> Result<(), ServiceError> {
    if ctx.override_access {
        return Ok(());
    }

    reject_unreadable_bulk_filters(ctx, hooks, scope.locale_ctx, filters)?;

    // Constraint hygiene (operator allowlist, system-column rules incl. the
    // `injecting_status` `_status` allowance, locale-scoped-field rejection)
    // happens inside `check_access` → `check_collection_access` — the single
    // validation chokepoint every access-resolving surface passes through.
    // Re-validating here would be a second, weaker copy of the same rules.
    let result = hooks.check_access(
        &AccessCheckInput::builder(scope.operation, ctx.slug)
            .access(scope.access_fn)
            .user(ctx.user)
            .data(scope.data)
            .injecting_status(scope.injecting_status)
            .build(),
    )?;

    match result {
        AccessResult::Allowed => Ok(()),
        AccessResult::Denied => {
            let mut op = scope.operation.to_string();
            if let Some(first) = op.get_mut(..1) {
                first.make_ascii_uppercase();
            }
            Err(ServiceError::AccessDenied(format!("{op} access denied")))
        }
        AccessResult::Constrained(extra) => {
            filters.extend(extra);
            Ok(())
        }
    }
}

/// A bulk write's outcome — its `modified` / `deleted` / `skipped` counts and
/// the `bulk_max_documents` "matched N" error — answers "how many rows match
/// this filter", so a filter on a field the caller may not read is an oracle
/// for that field's value, exactly as a find's results are. The caller's own
/// filters are judged by the read rule a find applies (the access hook's row
/// constraints, appended afterwards, are server-side and exempt), through
/// `strip` in the operation's locale (`locale_ctx`, the default locale
/// without one). The operation applies it through its write hooks; a queued
/// bulk run applies it at queue time too, so a request that can only be
/// refused is never stored.
///
/// # Errors
///
/// `AccessDenied` naming the first filter path the caller may not read.
pub(crate) fn reject_unreadable_bulk_filters(
    ctx: &ServiceContext,
    strip: &dyn FieldReadStrip,
    locale_ctx: Option<&LocaleContext>,
    filters: &[FilterClause],
) -> Result<(), ServiceError> {
    let default = ctx.default_locale_ctx();
    let locale = locale_ctx
        .or(default.as_ref())
        .map(LocaleContext::access_locale);

    reject_unreadable_filter_fields(ctx, strip, locale, filters)
}

/// The delete-family gate for the (possibly hard-delete-adjusted) definition:
/// soft delete → `access.trash ?? update`; permanent → `access.delete`. The
/// adjusted definition encodes force-hard-delete and trash purges, so the
/// derivation is uniform across surfaces.
pub(crate) fn delete_scope(def: &CollectionDefinition) -> BulkScope<'_> {
    if def.soft_delete {
        return BulkScope::builder("trash")
            .access_fn(def.access.resolve_trash())
            .build();
    }

    BulkScope::builder("delete")
        .access_fn(def.access.delete.as_ref())
        .build()
}

/// Restrict an update match-set to published rows unless the caller opted
/// into drafts (previously gRPC-only; now uniform).
pub(super) fn push_published_only_filter(
    def: &CollectionDefinition,
    draft: bool,
    filters: &mut Vec<FilterClause>,
) {
    if !draft && def.has_drafts() {
        filters.push(FilterClause::Single(Filter {
            field: "_status".to_string(),
            op: FilterOp::Equals("published".to_string()),
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The delete-family gate derives from the (possibly adjusted)
    /// definition: soft delete → trash gate (with its `?? update` fallback),
    /// permanent → `access.delete`.
    #[test]
    fn delete_scope_picks_trash_gate_when_soft_deleting_else_delete() {
        let mut def = CollectionDefinition::new("posts");
        def.soft_delete = true;
        def.access.delete = Some("can_delete".into());
        def.access.trash = Some("can_trash".into());

        let scope = delete_scope(&def);
        assert_eq!(scope.operation, "trash");
        assert_eq!(scope.access_fn.map(HookRef::reference), Some("can_trash"));

        def.soft_delete = false;
        let scope = delete_scope(&def);
        assert_eq!(scope.operation, "delete");
        assert_eq!(scope.access_fn.map(HookRef::reference), Some("can_delete"));
    }

    #[test]
    fn delete_scope_none_when_access_unset() {
        let mut def = CollectionDefinition::new("posts");
        assert!(delete_scope(&def).access_fn.is_none());
        def.soft_delete = true;
        assert!(delete_scope(&def).access_fn.is_none());
    }
}
