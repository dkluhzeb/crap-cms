//! Shared helper functions for the service layer.

use std::collections::HashMap;

use serde_json::Value;

use crate::{
    core::{Document, FieldDefinition, FieldType, collection::Hooks},
    db::{
        AccessResult, DbConnection, Filter, FilterClause, FilterOp, FindQuery, LocaleContext, query,
    },
    hooks::{HookContext, HookEvent},
    service::{AfterChangeInput, ServiceContext, ServiceError, hooks::WriteHooks},
};

/// Build the hook data map from form data + structured join data.
/// Converts string values to JSON strings and merges in blocks/arrays/has-many.
pub(crate) fn build_hook_data(
    data: &HashMap<String, String>,
    join_data: &HashMap<String, Value>,
) -> HashMap<String, Value> {
    let mut hook_data: HashMap<String, Value> = data
        .iter()
        .map(|(k, v)| (k.clone(), Value::String(v.clone())))
        .collect();
    for (k, v) in join_data {
        hook_data.insert(k.clone(), v.clone());
    }
    hook_data
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
) -> anyhow::Result<HashMap<String, Value>> {
    let mut after_data = doc.fields.clone();
    after_data.insert("id".to_string(), Value::String(doc.id.to_string()));
    let after_ctx = HookContext::builder(input.slug, input.operation)
        .data(after_data)
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

/// Collect field names that are marked top-level `hidden = true` for stripping
/// from API read responses (gRPC, Lua, MCP, admin JSON, REST). Includes group
/// subfields with `__` prefix. Mirrors the traversal pattern of
/// `collect_field_access_denied`.
///
/// `admin.hidden` is *not* read here — that flag controls admin-form rendering
/// only and never affects API output (so the admin upload widget, gRPC, Lua,
/// etc. can read auto-injected upload meta like `url`, `mime_type`, `focal_x`).
pub(crate) fn collect_api_hidden_field_names(
    fields: &[FieldDefinition],
    prefix: &str,
) -> Vec<String> {
    let mut hidden = Vec::new();

    for field in fields {
        let full_name = if prefix.is_empty() {
            field.name.clone()
        } else {
            format!("{}__{}", prefix, field.name)
        };

        if field.hidden {
            hidden.push(full_name.clone());
            continue; // parent hidden → skip sub-fields
        }

        match field.field_type {
            FieldType::Group => {
                hidden.extend(collect_api_hidden_field_names(&field.fields, &full_name));
            }
            FieldType::Row | FieldType::Collapsible => {
                hidden.extend(collect_api_hidden_field_names(&field.fields, prefix));
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    hidden.extend(collect_api_hidden_field_names(&tab.fields, prefix));
                }
            }
            _ => {}
        }
    }

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
    let def = ctx.collection_def();

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
    pub had_cursor: bool,
    pub cursor_has_more: Option<bool>,
}

/// Build a `PaginationResult` from query state, supporting both cursor and page modes.
///
/// Shared by `find_documents` and `search_documents` to avoid duplicating the
/// cursor/page branching logic.
pub(crate) fn build_pagination(inputs: PaginationInputs<'_>) -> query::PaginationResult {
    let limit = inputs.fq.limit.unwrap_or(inputs.total);

    if inputs.cursor_enabled {
        query::PaginationResult::builder(inputs.docs, inputs.total, limit).cursor(
            inputs.fq.order_by.as_deref(),
            inputs.has_timestamps,
            inputs.has_drafts,
            inputs.fq.before_cursor.is_some(),
            inputs.had_cursor,
            inputs.cursor_has_more,
        )
    } else {
        let offset = inputs.fq.offset.unwrap_or(0);
        let page = if limit > 0 { offset / limit + 1 } else { 1 };
        query::PaginationResult::builder(inputs.docs, inputs.total, limit).page(page, offset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{FieldAdmin, FieldType};

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

        assert_eq!(names, vec!["secret".to_string()]);
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

        assert_eq!(names, vec!["internal".to_string()]);
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

        assert_eq!(names, vec!["meta".to_string()]);
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

        assert_eq!(names, vec!["seo__internal_score".to_string()]);
    }
}
