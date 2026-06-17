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

use std::collections::HashSet;

use crate::{
    core::{FieldDefinition, prefixed_name, walk_leaf_fields},
    db::{Filter, FilterClause, FilterOp},
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
///
/// # Errors
///
/// Returns `HookError` for the first filter whose first dot-segment starts with `_`.
pub fn validate_user_filters(filters: &[FilterClause]) -> Result<(), ServiceError> {
    filters.iter().try_for_each(walk_user_filter)
}

/// Recursively check every leaf of a user-supplied [`FilterClause`] tree.
fn walk_user_filter(clause: &FilterClause) -> Result<(), ServiceError> {
    match clause {
        FilterClause::Single(f) => check_single_filter(f),
        FilterClause::And(subs) | FilterClause::Or(subs) => {
            subs.iter().try_for_each(walk_user_filter)
        }
    }
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
///
/// ## Operator allowlist — equality and membership only
///
/// Access rules may only use `equals` / `not_equals` / `in` / `not_in` /
/// `exists` / `not_exists`. Pattern operators (`like`, `contains`) and ordered
/// operators (`greater_than`, `less_than`, `greater_than_or_equal`,
/// `less_than_or_equal`) are **rejected**.
///
/// The reason is the dual evaluation path: every access constraint is both
/// compiled to a SQL `WHERE` clause *and* evaluated in-memory (live event
/// streams, relationship/join population) — and pattern/ordered matching can
/// diverge between SQL and in-memory (collation, `LIKE` semantics, type
/// affinity), with the default bias toward leaking. Restricting access rules to
/// equality + membership makes that divergence unrepresentable by construction.
/// It loses no real expressiveness: access control is fundamentally identity /
/// ownership / membership ("is this row *yours* / your *tenant's* / in your
/// *set*"), which equality + membership express exactly. Value-querying (ranges,
/// prefixes, substrings) belongs in *user* filters, which keep the full algebra.
/// See `docs/src/access-control/filter-constraints.md` for worked examples.
///
/// # Errors
///
/// Returns `HookError` for the first filter that either (a) targets a system
/// column outside the allowed exceptions (`_deleted_at` in trash mode,
/// `_status` when injecting status), or (b) uses a pattern/ordered operator.
pub fn validate_access_constraints(
    filters: &[FilterClause],
    trash: bool,
    injecting_status: bool,
    slug: &str,
) -> Result<(), ServiceError> {
    filters
        .iter()
        .try_for_each(|c| walk_access_filter(c, trash, injecting_status, slug))
}

/// Recursively check every leaf of an access-hook [`FilterClause`] tree against
/// the constrained system-column rule.
fn walk_access_filter(
    clause: &FilterClause,
    trash: bool,
    injecting_status: bool,
    slug: &str,
) -> Result<(), ServiceError> {
    match clause {
        FilterClause::Single(f) => check_access_filter(f, trash, injecting_status, slug),
        FilterClause::And(subs) | FilterClause::Or(subs) => subs
            .iter()
            .try_for_each(|c| walk_access_filter(c, trash, injecting_status, slug)),
    }
}

/// Check a single access-hook filter against the constrained system-column rule,
/// the equality/membership operator allowlist, and the flat-field rule.
fn check_access_filter(
    filter: &Filter,
    trash: bool,
    injecting_status: bool,
    slug: &str,
) -> Result<(), ServiceError> {
    check_access_operator(filter, slug)?;

    let field = &filter.field;

    // A dotted relationship/JSON path (`author.id`, `meta.tags`) resolves via a
    // SQL subquery, but the in-memory matcher (live events, populated targets)
    // only sees flat fields and fails closed on it — a divergence. Reject it so
    // the two paths can never disagree; denormalize to a flat own column
    // (e.g. `author_id`) instead.
    if field.contains('.') {
        return Err(ServiceError::HookError(format!(
            "Access hook for '{slug}' constrains the path '{field}' — access rules must reference a flat own column, not a dotted relationship/JSON path (it evaluates inconsistently across the SQL and in-memory enforcement paths). Denormalize to a flat field (e.g. 'author_id') instead."
        )));
    }

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

/// Reject pattern (`like` / `contains`) and ordered (`greater_than` etc.)
/// operators in an access constraint. Access rules may only use equality and
/// membership; see [`validate_access_constraints`] for the rationale (the dual
/// SQL / in-memory evaluation path can diverge for these operators and bias
/// toward leaking).
fn check_access_operator(filter: &Filter, slug: &str) -> Result<(), ServiceError> {
    let disallowed = match &filter.op {
        FilterOp::Like(_) => "like",
        FilterOp::Contains(_) => "contains",
        FilterOp::GreaterThan(_) => "greater_than",
        FilterOp::LessThan(_) => "less_than",
        FilterOp::GreaterThanOrEqual(_) => "greater_than_or_equal",
        FilterOp::LessThanOrEqual(_) => "less_than_or_equal",
        FilterOp::Equals(_)
        | FilterOp::NotEquals(_)
        | FilterOp::In(_)
        | FilterOp::NotIn(_)
        | FilterOp::Exists
        | FilterOp::NotExists => return Ok(()),
    };

    let field = &filter.field;
    Err(ServiceError::HookError(format!(
        "Access hook for '{slug}' used the '{disallowed}' operator on '{field}', but access rules may only use equality and membership operators (equals, not_equals, in, not_in, exists, not_exists). Pattern and ordered operators evaluate differently across SQL and in-memory enforcement and could leak — model the constraint as an exact field match or a membership set instead."
    )))
}

/// Reject an access constraint that targets a locale-scoped (localized) field.
///
/// A localized field is stored per-locale (`field__locale`). The SQL path
/// resolves such a constraint to a *specific* locale column, while the in-memory
/// path (live events, populated relationships) matches a flat / active-locale
/// value keyed by the logical name — so the two can diverge. Access rules
/// express identity / ownership / membership, never a localized *content* value
/// (`{ title = X }` is ambiguous — which locale?), so a constraint on a
/// locale-scoped field is rejected.
///
/// `fields` is the constrained collection's (or global's) field list, used to
/// determine which flat field names are locale-scoped (including fields under a
/// localized Group, via `is_locale_scoped`).
///
/// # Errors
///
/// Returns `HookError` for the first constraint whose field is locale-scoped.
pub fn validate_access_constraint_locales(
    filters: &[FilterClause],
    fields: &[FieldDefinition],
    slug: &str,
) -> Result<(), ServiceError> {
    let mut localized: HashSet<String> = HashSet::new();
    let _ = walk_leaf_fields(fields, "", false, &mut |field, prefix, inherited| {
        if field.is_locale_scoped(inherited) {
            localized.insert(prefixed_name(prefix, &field.name));
        }
        Ok(())
    });

    if localized.is_empty() {
        return Ok(());
    }

    filters
        .iter()
        .try_for_each(|c| walk_locale_filter(c, &localized, slug))
}

/// Recursively reject any leaf filter whose field is locale-scoped.
fn walk_locale_filter(
    clause: &FilterClause,
    localized: &HashSet<String>,
    slug: &str,
) -> Result<(), ServiceError> {
    match clause {
        FilterClause::Single(f) => {
            let first = f.field.split('.').next().unwrap_or(&f.field);
            if !localized.contains(first) {
                return Ok(());
            }
            Err(ServiceError::HookError(format!(
                "Access hook for '{slug}' constrains the localized field '{}' — access rules must reference non-localized (shared) fields. A localized field is stored per-locale and evaluates inconsistently across the SQL and in-memory enforcement paths; use a non-localized identity/ownership field instead.",
                f.field
            )))
        }
        FilterClause::And(subs) | FilterClause::Or(subs) => subs
            .iter()
            .try_for_each(|c| walk_locale_filter(c, localized, slug)),
    }
}

#[cfg(test)]
mod tests {
    use crate::core::FieldType;
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
        let filters = vec![FilterClause::or_groups(vec![
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

    // ── operator allowlist (equality + membership only) ────────────────

    fn single_op(field: &str, op: FilterOp) -> FilterClause {
        FilterClause::Single(Filter {
            field: field.to_string(),
            op,
        })
    }

    /// Equality and membership operators are the allowed set for access rules.
    #[test]
    fn validates_access_constraints_allows_equality_and_membership_ops() {
        let ok = vec![
            single_op("author_id", FilterOp::Equals("u1".into())),
            single_op("author_id", FilterOp::NotEquals("u1".into())),
            single_op("dept", FilterOp::In(vec!["a".into(), "b".into()])),
            single_op("dept", FilterOp::NotIn(vec!["a".into()])),
            single_op("deleted_marker", FilterOp::Exists),
            single_op("deleted_marker", FilterOp::NotExists),
        ];
        for f in ok {
            assert!(
                validate_access_constraints(&[f], false, false, "posts").is_ok(),
                "equality/membership operator must be allowed in an access rule"
            );
        }
    }

    /// Pattern and ordered operators are rejected: their SQL vs in-memory
    /// evaluation can diverge and bias toward leaking. Regression for the
    /// strict access-operator allowlist.
    #[test]
    fn validates_access_constraints_rejects_pattern_and_ordered_ops() {
        let bad = [
            ("like", single_op("path", FilterOp::Like("eng/%".into()))),
            (
                "contains",
                single_op("tags", FilterOp::Contains("x".into())),
            ),
            (
                "greater_than",
                single_op("created_at", FilterOp::GreaterThan("0".into())),
            ),
            (
                "less_than",
                single_op("price", FilterOp::LessThan("9".into())),
            ),
            (
                "greater_than_or_equal",
                single_op("level", FilterOp::GreaterThanOrEqual("3".into())),
            ),
            (
                "less_than_or_equal",
                single_op("level", FilterOp::LessThanOrEqual("3".into())),
            ),
        ];
        for (name, f) in bad {
            let err = validate_access_constraints(&[f], false, false, "posts").unwrap_err();
            let msg = err.to_string();
            assert!(matches!(err, ServiceError::HookError(_)));
            assert!(
                msg.contains(name),
                "error should name the '{name}' operator: {msg}"
            );
            assert!(msg.contains("posts"), "error should name the slug: {msg}");
        }
    }

    /// A dotted relationship/JSON path is rejected in an access constraint — it
    /// diverges between the SQL (subquery) and in-memory (flat) paths.
    #[test]
    fn validates_access_constraints_rejects_dotted_path() {
        let filters = vec![single_op("author.id", FilterOp::Equals("u1".into()))];
        let err = validate_access_constraints(&filters, false, false, "posts").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("author.id"), "{msg}");
        assert!(msg.contains("flat"), "{msg}");
    }

    /// The operator allowlist is enforced even on a system column whose name
    /// would otherwise be allowed — the operator is checked first.
    #[test]
    fn validates_access_constraints_rejects_ordered_op_on_allowed_system_column() {
        let filters = vec![single_op("_status", FilterOp::GreaterThan("draft".into()))];
        let err = validate_access_constraints(&filters, false, true, "posts").unwrap_err();
        assert!(err.to_string().contains("greater_than"));
    }

    /// The allowlist is walked into OR/AND groups, not just top-level leaves.
    #[test]
    fn validates_access_constraints_rejects_pattern_op_inside_or_group() {
        let filters = vec![FilterClause::or_groups(vec![
            vec![Filter {
                field: "author_id".to_string(),
                op: FilterOp::Equals("u1".to_string()),
            }],
            vec![Filter {
                field: "path".to_string(),
                op: FilterOp::Like("eng/%".to_string()),
            }],
        ])];
        let err = validate_access_constraints(&filters, false, false, "posts").unwrap_err();
        assert!(err.to_string().contains("like"));
    }

    // ── locale-scoped field rejection (M3) ─────────────────────────────

    /// Regression: an access constraint on a localized field is rejected, since
    /// it evaluates inconsistently across SQL (per-locale column) and in-memory
    /// (flat/active-locale) enforcement. A non-localized field is allowed.
    #[test]
    fn validates_access_constraints_rejects_localized_field() {
        let mut title = FieldDefinition::builder("title", FieldType::Text).build();
        title.localized = true;
        let owner = FieldDefinition::builder("owner", FieldType::Text).build();
        let fields = vec![title, owner];

        let bad = vec![single_op("title", FilterOp::Equals("x".into()))];
        let err = validate_access_constraint_locales(&bad, &fields, "posts").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("localized"), "{msg}");
        assert!(msg.contains("title"), "{msg}");

        let ok = vec![single_op("owner", FilterOp::Equals("u1".into()))];
        assert!(validate_access_constraint_locales(&ok, &fields, "posts").is_ok());
    }

    /// A shared field under a localized Group is itself locale-scoped (its column
    /// is suffixed), so constraining it is rejected too.
    #[test]
    fn validates_access_constraints_rejects_field_in_localized_group() {
        let mut inner = FieldDefinition::builder("color", FieldType::Text).build();
        // not directly localized — inherits from the group
        inner.localized = false;
        let mut group = FieldDefinition::builder("meta", FieldType::Group).build();
        group.localized = true;
        group.fields = vec![inner];
        let fields = vec![group];

        let bad = vec![single_op("meta__color", FilterOp::Equals("red".into()))];
        let err = validate_access_constraint_locales(&bad, &fields, "posts").unwrap_err();
        assert!(err.to_string().contains("meta__color"));
    }

    /// OR groups are walked just like in `validate_user_filters`.
    #[test]
    fn validates_access_constraints_walks_or_groups() {
        let filters = vec![FilterClause::or_groups(vec![
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
