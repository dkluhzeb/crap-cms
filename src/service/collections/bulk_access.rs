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
    db::{AccessResult, Filter, FilterClause, FilterOp},
    hooks::AccessCheckInput,
    service::{ServiceContext, ServiceError, hooks::WriteHooks},
};

/// What a bulk operation is gated by. All fields required; constructed at the
/// four call sites (pool/conn × update/delete) — plain struct literal.
pub(super) struct BulkScope<'a> {
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
}

/// Gate the bulk operation and scope its match-set: run the access hook once
/// up front — `Denied` errors before anything is matched, `Constrained`
/// appends the row filters so out-of-scope rows are never matched. Skipped
/// under `override_access`, matching the single-op paths. The per-document
/// lifecycle still enforces per-doc access; this bounds WHICH rows are
/// matched and provides the early denial.
pub(super) fn scope_bulk_access(
    ctx: &ServiceContext,
    hooks: &dyn WriteHooks,
    scope: &BulkScope<'_>,
    filters: &mut Vec<FilterClause>,
) -> Result<(), ServiceError> {
    if ctx.override_access {
        return Ok(());
    }

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

/// The delete-family gate for the (possibly hard-delete-adjusted) definition:
/// soft delete → `access.trash ?? update`; permanent → `access.delete`. The
/// adjusted definition encodes force-hard-delete and trash purges, so the
/// derivation is uniform across surfaces.
pub(super) fn delete_scope(def: &CollectionDefinition) -> BulkScope<'_> {
    if def.soft_delete {
        BulkScope {
            operation: "trash",
            access_fn: def.access.resolve_trash(),
            data: None,
            injecting_status: false,
        }
    } else {
        BulkScope {
            operation: "delete",
            access_fn: def.access.delete.as_ref(),
            data: None,
            injecting_status: false,
        }
    }
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
