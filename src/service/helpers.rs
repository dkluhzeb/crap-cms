//! Shared helper functions for the service layer.

use serde_json::Value;

use crate::{
    config::PasswordPolicy,
    core::{
        Document, FieldDefinition, FieldDenial, ReqContext,
        collection::Hooks,
        validate::{FieldError, ValidationError},
    },
    db::{
        AccessResult, DbConnection, Filter, FilterClause, FilterOp, FindQuery, LocaleContext, query,
    },
    hooks::{HookContext, HookEvent, lifecycle::access::collect_denials_flat},
    service::{AfterChangeInput, ServiceContext, ServiceError, hooks::WriteHooks},
};

/// Validate a supplied auth-collection `password` against the effective policy,
/// surfaced as a structured `password` field error.
///
/// This is THE authoritative password-policy enforcement point: the service
/// create/update chokepoint calls it for every surface and every op (single and
/// bulk), so no weak password can reach the DB regardless of which caller wrote
/// it. A `None` policy falls back to [`PasswordPolicy::default`] — the policy is
/// *always* enforced, so a context that forgets to thread the configured policy
/// degrades to the default rules, never to no enforcement. No-op for a non-auth
/// collection, an absent password, or an empty password (empty = "no change" on
/// update; create requires a non-empty password upstream).
///
/// # Errors
///
/// Returns [`ServiceError::Validation`] with a single `password` field error
/// when the password violates the policy.
pub(crate) fn validate_password_policy(
    is_auth: bool,
    password: Option<&str>,
    policy: Option<&PasswordPolicy>,
) -> Result<(), ServiceError> {
    if !is_auth {
        return Ok(());
    }

    let Some(pw) = password.filter(|p| !p.is_empty()) else {
        return Ok(());
    };

    let default_policy = PasswordPolicy::default();
    let policy = policy.unwrap_or(&default_policy);

    policy.validate(pw).map_err(|e| {
        ServiceError::Validation(ValidationError::new(vec![FieldError::with_key(
            "password",
            e.to_string(),
            "validation.password_policy",
        )]))
    })
}

/// Run after-change hooks and return the request-scoped context.
/// This pattern is repeated across create, update, unpublish, and global update.
pub(crate) fn run_after_change_hooks(
    write_hooks: &dyn WriteHooks,
    hooks: &Hooks,
    fields: &[FieldDefinition],
    doc: &Document,
    input: AfterChangeInput<'_>,
    tx: &dyn DbConnection,
) -> anyhow::Result<ReqContext> {
    let mut after_data = doc.fields.clone();
    after_data.insert("id".to_string(), Value::String(doc.id.to_string()));
    let after_ctx = HookContext::builder(input.slug, input.operation)
        .data(after_data)
        .document_id(doc.id.to_string())
        .draft(input.is_draft)
        .locale(input.locale)
        .context(input.req_context)
        .user(input.user)
        .ui_locale(input.ui_locale)
        .build();
    let after_result =
        write_hooks.run_after_write(hooks, fields, HookEvent::AfterChange, after_ctx, tx)?;
    Ok(after_result.context)
}

/// Collect denials for fields marked top-level `hidden = true`, for stripping
/// from API read responses (gRPC, Lua, MCP, admin JSON, REST). Covers flat
/// columns, group subfields (`__` prefix), and fields nested inside array/blocks
/// rows at any depth — sharing the [`collect_denials_flat`] walker with
/// field-access so the two never diverge.
///
/// `admin.hidden` is *not* read here — that flag controls admin-form rendering
/// only and never affects API output (so the admin upload widget, gRPC, Lua,
/// etc. can read auto-injected upload meta like `url`, `mime_type`, `focal_x`).
pub(crate) fn collect_api_hidden_field_names(
    fields: &[FieldDefinition],
    prefix: &str,
) -> Vec<FieldDenial> {
    let is_hidden = |field: &FieldDefinition| field.hidden;

    let mut hidden = Vec::new();
    collect_denials_flat(fields, &is_hidden, prefix, &mut hidden);

    hidden
}

/// Enforce a write-access `Constrained` result against a specific target row.
///
/// When a write access hook returns [`AccessResult::Constrained(filters)`],
/// operators expect the extra filters to scope the write to matching rows
/// (e.g. "users can only update their own rows"). The write paths have no
/// in-memory filter evaluator, so this helper piggybacks on the query layer:
/// it counts rows matching `filters AND id = <id>` and rejects the write
/// (returns [`ServiceError::AccessDenied`]) when zero rows match.
///
/// Non-`Constrained` variants are a no-op — callers handle `Allowed`/`Denied`
/// themselves before the write. `locale_ctx` is passed as `None` because
/// access-hook constraints are almost always locale-independent identity
/// filters (`author_id = X`), and the target row exists in some locale.
///
/// `include_deleted` must be true for undelete (the target row is in the
/// trash) and false everywhere else. `operation` is used only for the error
/// message ("Update access denied", "Delete access denied", …).
pub(crate) fn enforce_access_constraints(
    ctx: &ServiceContext,
    id: &str,
    access: &AccessResult,
    operation: &str,
    include_deleted: bool,
) -> Result<(), ServiceError> {
    let AccessResult::Constrained(extra) = access else {
        return Ok(());
    };

    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();
    let def = ctx.collection_def()?;

    let mut filters: Vec<FilterClause> = extra.clone();
    filters.push(FilterClause::Single(Filter {
        field: "id".to_string(),
        op: FilterOp::Equals(id.to_string()),
    }));

    let locale_ctx: Option<&LocaleContext> = None;
    let matched = query::count_with_search(
        conn,
        ctx.slug,
        def,
        &filters,
        locale_ctx,
        None,
        include_deleted,
    )?;

    if matched == 0 {
        return Err(ServiceError::AccessDenied(format!(
            "{operation} access denied"
        )));
    }

    Ok(())
}

/// Inputs for [`build_pagination`]. Grouped into a struct per
/// CLAUDE.md's "more than 4 parameters" rule; constructed at the two
/// call sites in the read service (`find_documents`,
/// `search_documents`).
pub(crate) struct PaginationInputs<'a> {
    pub docs: &'a [Document],
    pub total: i64,
    pub fq: &'a FindQuery,
    pub cursor_enabled: bool,
    pub has_timestamps: bool,
    /// Whether the collection has drafts enabled — controls cursor
    /// `status_val` encoding for the composite ordering surfaced by
    /// `apply_order_by`.
    pub has_drafts: bool,
    pub cursor_has_more: Option<bool>,
}

/// Build a `PaginationResult` from query state, supporting both cursor and page modes.
///
/// Shared by `find_documents` and `search_documents` to avoid duplicating the
/// cursor/page branching logic.
pub(crate) fn build_pagination(inputs: &PaginationInputs<'_>) -> query::PaginationResult {
    let limit = inputs.fq.limit.unwrap_or(inputs.total);

    if inputs.cursor_enabled {
        query::PaginationResult::builder(inputs.docs, inputs.total, limit).cursor(
            inputs.fq.order_by.as_deref(),
            query::CursorFlags {
                has_timestamps: inputs.has_timestamps,
                has_drafts: inputs.has_drafts,
                had_before_cursor: inputs.fq.before_cursor.is_some(),
                had_any_cursor: inputs.fq.after_cursor.is_some()
                    || inputs.fq.before_cursor.is_some(),
                cursor_has_more: inputs.cursor_has_more,
            },
        )
    } else {
        let offset = inputs.fq.offset.unwrap_or(0);
        let page = if limit > 0 { offset / limit + 1 } else { 1 };
        query::PaginationResult::builder(inputs.docs, inputs.total, limit).page(page, offset)
    }
}

/// Bump the query limit by one when keyset (cursor) pagination must peek at the
/// next row to decide `has_more`. Returns whether overfetch is active — the
/// caller passes that flag back to [`finish_cursor_overfetch`] after fetching.
/// Shared by `find_documents` and `search_documents`.
pub(crate) fn begin_cursor_overfetch(fq: &mut FindQuery, cursor_enabled: bool) -> bool {
    let had_cursor = fq.after_cursor.is_some() || fq.before_cursor.is_some();
    let overfetch = cursor_enabled && had_cursor;

    if overfetch {
        fq.limit = fq.limit.map(|l| l + 1);
    }

    overfetch
}

/// Undo [`begin_cursor_overfetch`]'s limit bump and trim the peeked extra row
/// (the first row when paging backward, else the last). Returns `Some(has_more)`
/// when cursor pagination is active, else `None`. Shared by `find_documents` and
/// `search_documents`.
pub(crate) fn finish_cursor_overfetch(
    fq: &mut FindQuery,
    docs: &mut Vec<Document>,
    overfetch: bool,
    total: i64,
) -> Option<bool> {
    // Restore the original limit for pagination math.
    if overfetch {
        fq.limit = fq.limit.map(|l| l - 1);
    }

    if !overfetch {
        return None;
    }

    let limit = fq.limit.unwrap_or(total);

    // Saturate the doc count for the unreachable case so the `>` check holds.
    let docs_count = i64::try_from(docs.len()).unwrap_or(i64::MAX);
    if docs_count <= limit {
        return Some(false);
    }

    if fq.before_cursor.is_some() {
        docs.remove(0);
    } else {
        docs.pop();
    }

    Some(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{FieldAdmin, FieldType};

    // ── validate_password_policy ──────────────────────────────────────

    #[test]
    fn password_policy_non_auth_is_skipped() {
        // A non-auth collection may carry a legitimate `password` field; the
        // policy never applies to it.
        assert!(validate_password_policy(false, Some("x"), None).is_ok());
    }

    #[test]
    fn password_policy_absent_or_empty_is_skipped() {
        // Absent password, and empty password ("no change" on update), skip.
        assert!(validate_password_policy(true, None, None).is_ok());
        assert!(validate_password_policy(true, Some(""), None).is_ok());
    }

    #[test]
    fn password_policy_weak_rejected_as_password_field_error() {
        // `None` policy falls back to the default (min length 8): a short
        // password is rejected as a structured `password` field error, so every
        // surface renders it on the password input.
        let err = validate_password_policy(true, Some("short"), None).unwrap_err();
        match err {
            ServiceError::Validation(ve) => {
                assert_eq!(ve.errors.len(), 1);
                assert_eq!(ve.errors[0].field, "password");
                assert_eq!(
                    ve.errors[0].key.as_deref(),
                    Some("validation.password_policy")
                );
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[test]
    fn password_policy_valid_passes() {
        assert!(validate_password_policy(true, Some("longenough"), None).is_ok());
    }

    #[test]
    fn password_policy_none_falls_back_to_default_never_skips() {
        // The fail-safe: a context that forgets to thread a policy still enforces
        // the DEFAULT policy — never no enforcement.
        assert!(validate_password_policy(true, Some("weak"), None).is_err());
    }

    #[test]
    fn password_policy_uses_threaded_policy_over_default() {
        // A stricter configured policy applies when threaded.
        let strict = PasswordPolicy {
            min_length: 12,
            ..PasswordPolicy::default()
        };
        // Passes default (>=8) but fails the stricter threaded policy (>=12).
        assert!(validate_password_policy(true, Some("longenough"), Some(&strict)).is_err());
    }

    /// Helper: build a Text field with the given hidden flags.
    fn text_field(name: &str, hidden: bool, admin_hidden: bool) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Text)
            .hidden(hidden)
            .admin(FieldAdmin::builder().hidden(admin_hidden).build())
            .build()
    }

    /// Top-level `hidden = true` → field is collected for API stripping.
    #[test]
    fn collects_top_level_hidden_field() {
        let fields = vec![text_field("secret", true, false)];

        let names = collect_api_hidden_field_names(&fields, "");

        assert_eq!(names, vec![FieldDenial::Flat("secret".into())]);
    }

    /// `admin.hidden = true` (only) → NOT collected. This is the upload-bug
    /// fix: `admin.hidden` is a rendering flag, not a data-visibility flag.
    #[test]
    fn does_not_collect_admin_hidden_only() {
        let fields = vec![text_field("url", false, true)];

        let names = collect_api_hidden_field_names(&fields, "");

        assert!(
            names.is_empty(),
            "admin.hidden alone must not strip from API responses"
        );
    }

    /// Both flags set → still collected (top-level wins; admin.hidden is redundant but legal).
    #[test]
    fn collects_when_both_flags_set() {
        let fields = vec![text_field("internal", true, true)];

        let names = collect_api_hidden_field_names(&fields, "");

        assert_eq!(names, vec![FieldDenial::Flat("internal".into())]);
    }

    /// Default field (neither flag) → not collected.
    #[test]
    fn does_not_collect_visible_field() {
        let fields = vec![text_field("title", false, false)];

        let names = collect_api_hidden_field_names(&fields, "");

        assert!(names.is_empty());
    }

    /// Group with `hidden = true` parent → parent name returned, subfields skipped
    /// (parent-hidden short-circuit preserved from the original implementation).
    #[test]
    fn hidden_group_parent_skips_subfields() {
        let group = FieldDefinition::builder("meta", FieldType::Group)
            .hidden(true)
            .fields(vec![text_field("inner", false, false)])
            .build();

        let names = collect_api_hidden_field_names(&[group], "");

        assert_eq!(names, vec![FieldDenial::Flat("meta".into())]);
    }

    /// Group with visible parent but hidden subfield → subfield collected with
    /// `parent__child` prefix.
    #[test]
    fn visible_group_collects_hidden_subfields_with_prefix() {
        let group = FieldDefinition::builder("seo", FieldType::Group)
            .fields(vec![
                text_field("title", false, false),
                text_field("internal_score", true, false),
            ])
            .build();

        let names = collect_api_hidden_field_names(&[group], "");

        assert_eq!(names, vec![FieldDenial::Flat("seo__internal_score".into())]);
    }
}
