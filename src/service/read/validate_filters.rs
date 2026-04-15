//! Service-layer filter validation: reject user-supplied filters on system columns.
//!
//! System columns (field paths starting with `_`) are engine-internal. The public
//! read APIs (gRPC, Lua, admin, MCP) funnel all read operations through the service
//! layer, so this module is the single place where the security rule is enforced —
//! parse sites are pure parsers.
//!
//! The rule is *flat*: any user-supplied filter whose first dot-segment starts
//! with `_` is rejected, with no exceptions. System filters (`_status`,
//! `_deleted_at`) are not user-supplied — they are injected by the service layer
//! itself *after* validation, in response to typed request flags
//! (`trash = true` / `include_drafts = true`). That keeps the validator simple
//! and prevents user filters from silently producing empty results when they
//! happen to mention an internal column.
//!
//! ## Populate cache + singleflight access-leak guardrail
//!
//! Related invariant (enforced in `post_process.rs::effective_populate_state`):
//! `override_access` callers (Lua `opts.overrideAccess = true`, MCP) disable the
//! shared populate cache **and** the process-wide populate singleflight. Those
//! callers bypass collection-level access hooks, so a doc they fetched could
//! leak into another user's populate cache lookup — a subsequent read's cache
//! hit happens *after* access evaluation per-doc, but the cached doc itself
//! wasn't re-checked against the second user's access. Zeroing both cache and
//! singleflight at the populate entry point is a single-chokepoint rule that
//! applies uniformly regardless of what the caller's input struct carries.
//! Override-access fetches are still deduplicated within their own call via
//! the fresh per-call singleflight created by `populate_relationships_*`.

use crate::{
    db::{Filter, FilterClause},
    service::ServiceError,
};

/// Validate that user-supplied filter clauses do not target system columns.
///
/// Walks both top-level filters and nested OR groups. Returns the first offending
/// field path wrapped in a [`ServiceError::HookError`] (which maps to
/// `InvalidArgument` at the gRPC boundary and a user-facing message on all other
/// surfaces).
///
/// Any field path whose first dot-segment starts with `_` is rejected. To reach
/// system-scoped data (trash, drafts), callers must use the typed request flags;
/// the service layer injects the corresponding system filters post-validation.
pub fn validate_user_filters(filters: &[FilterClause]) -> Result<(), ServiceError> {
    for clause in filters {
        match clause {
            FilterClause::Single(f) => check_single_filter(f)?,
            FilterClause::Or(groups) => {
                for group in groups {
                    for f in group {
                        check_single_filter(f)?;
                    }
                }
            }
        }
    }

    Ok(())
}

/// Check a single filter's field against the system-column rule.
fn check_single_filter(filter: &Filter) -> Result<(), ServiceError> {
    let field = &filter.field;
    let first = field.split('.').next().unwrap_or(field);

    if !first.starts_with('_') {
        return Ok(());
    }

    Err(ServiceError::HookError(format!(
        "Cannot filter on system column '{field}' — reach this data via the 'trash' or 'draft' request flag instead."
    )))
}

/// Validate filter clauses returned by a Lua access hook via
/// [`AccessResult::Constrained`](crate::db::AccessResult::Constrained).
///
/// Operator-written access hooks are trusted more than user-supplied filters,
/// so the rule is looser than [`validate_user_filters`]: system columns are
/// allowed when the service is already injecting the matching filter.
///
/// - `_deleted_at` is allowed only when `trash == true` (the service is
///   filtering the trash view itself).
/// - `_status` is allowed only when `injecting_status == true`, which means
///   the service has already pushed `_status = 'published'` (drafts are
///   excluded for this request and the collection has drafts).
/// - Any other `_*` top-level field is rejected with a clear, operator-facing
///   error that names the `slug` and offending field — a typo on a system
///   column name (e.g. `_password_hash` instead of `password_hash_status`)
///   would otherwise silently break the query or leak data.
pub fn validate_access_constraints(
    filters: &[FilterClause],
    trash: bool,
    injecting_status: bool,
    slug: &str,
) -> Result<(), ServiceError> {
    for clause in filters {
        match clause {
            FilterClause::Single(f) => check_access_filter(f, trash, injecting_status, slug)?,
            FilterClause::Or(groups) => {
                for group in groups {
                    for f in group {
                        check_access_filter(f, trash, injecting_status, slug)?;
                    }
                }
            }
        }
    }

    Ok(())
}

/// Check a single access-hook filter against the constrained system-column rule.
fn check_access_filter(
    filter: &Filter,
    trash: bool,
    injecting_status: bool,
    slug: &str,
) -> Result<(), ServiceError> {
    let field = &filter.field;
    let first = field.split('.').next().unwrap_or(field);

    if !first.starts_with('_') {
        return Ok(());
    }

    if first == "_deleted_at" && trash {
        return Ok(());
    }

    if first == "_status" && injecting_status {
        return Ok(());
    }

    Err(ServiceError::HookError(format!(
        "Access hook for '{slug}' returned a filter on system column '{field}' — this is almost always a typo; use a non-system column or remove the constraint."
    )))
}

#[cfg(test)]
mod tests {
    use crate::db::{Filter, FilterClause, FilterOp};

    use super::*;

    fn single(field: &str) -> FilterClause {
        FilterClause::Single(Filter {
            field: field.to_string(),
            op: FilterOp::Equals("x".to_string()),
        })
    }

    #[test]
    fn validates_allows_normal_column() {
        let filters = vec![single("title")];
        assert!(validate_user_filters(&filters).is_ok());
    }

    #[test]
    fn validates_rejects_underscore_column() {
        let filters = vec![single("_password_hash")];
        let err = validate_user_filters(&filters).unwrap_err();
        assert!(matches!(err, ServiceError::HookError(_)));
    }

    /// `_deleted_at` is a system column. Even though the service layer injects it
    /// when `trash = true`, user-supplied filters on it are rejected — the typed
    /// flag is the only supported entry point.
    #[test]
    fn validates_rejects_deleted_at() {
        let filters = vec![single("_deleted_at")];
        assert!(validate_user_filters(&filters).is_err());
    }

    /// `_status` is similarly off-limits to user filters; the `draft` flag is
    /// the supported way to control draft visibility.
    #[test]
    fn validates_rejects_status() {
        let filters = vec![single("_status")];
        assert!(validate_user_filters(&filters).is_err());
    }

    #[test]
    fn validates_walks_or_groups() {
        let filters = vec![FilterClause::Or(vec![
            vec![Filter {
                field: "title".to_string(),
                op: FilterOp::Equals("ok".to_string()),
            }],
            vec![Filter {
                field: "_ref_count".to_string(),
                op: FilterOp::GreaterThan("0".to_string()),
            }],
        ])];

        let err = validate_user_filters(&filters).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("_ref_count"), "error: {msg}");
    }

    #[test]
    fn validates_error_names_the_field() {
        let filters = vec![single("_locked")];
        let err = validate_user_filters(&filters).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("_locked"), "error: {msg}");
        assert!(msg.contains("system column"), "error: {msg}");
    }

    /// Nested dot paths whose first segment is a user field are fine — the
    /// underscore guard only applies to the top-level system columns.
    #[test]
    fn validates_allows_user_dot_paths_with_underscore_sub_segments() {
        let filters = vec![single("content._block_type")];
        assert!(validate_user_filters(&filters).is_ok());
    }

    // ── validate_access_constraints ────────────────────────────────────

    #[test]
    fn validates_access_constraints_allows_normal_column() {
        let filters = vec![single("author_id")];
        assert!(validate_access_constraints(&filters, false, false, "posts").is_ok());
    }

    #[test]
    fn validates_access_constraints_rejects_underscore_column() {
        let filters = vec![single("_password_hash")];
        let err = validate_access_constraints(&filters, false, false, "users").unwrap_err();
        assert!(matches!(err, ServiceError::HookError(_)));
    }

    #[test]
    fn validates_access_constraints_allows_deleted_at_in_trash_mode() {
        let filters = vec![single("_deleted_at")];
        assert!(validate_access_constraints(&filters, true, false, "posts").is_ok());
    }

    #[test]
    fn validates_access_constraints_rejects_deleted_at_outside_trash() {
        let filters = vec![single("_deleted_at")];
        let err = validate_access_constraints(&filters, false, false, "posts").unwrap_err();
        assert!(matches!(err, ServiceError::HookError(_)));
    }

    #[test]
    fn validates_access_constraints_allows_status_when_injecting_status() {
        let filters = vec![single("_status")];
        assert!(validate_access_constraints(&filters, false, true, "posts").is_ok());
    }

    #[test]
    fn validates_access_constraints_rejects_status_otherwise() {
        let filters = vec![single("_status")];
        let err = validate_access_constraints(&filters, false, false, "posts").unwrap_err();
        assert!(matches!(err, ServiceError::HookError(_)));
    }

    #[test]
    fn validates_access_constraints_error_names_slug_and_field() {
        let filters = vec![single("_ref_count")];
        let err = validate_access_constraints(&filters, false, false, "articles").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("articles"), "error: {msg}");
        assert!(msg.contains("_ref_count"), "error: {msg}");
        assert!(msg.contains("system column"), "error: {msg}");
        assert!(msg.contains("typo"), "error: {msg}");
    }

    /// OR groups are walked just like in `validate_user_filters`.
    #[test]
    fn validates_access_constraints_walks_or_groups() {
        let filters = vec![FilterClause::Or(vec![
            vec![Filter {
                field: "author_id".to_string(),
                op: FilterOp::Equals("u1".to_string()),
            }],
            vec![Filter {
                field: "_locked".to_string(),
                op: FilterOp::Equals("0".to_string()),
            }],
        ])];
        let err = validate_access_constraints(&filters, false, false, "posts").unwrap_err();
        assert!(err.to_string().contains("_locked"));
    }
}
