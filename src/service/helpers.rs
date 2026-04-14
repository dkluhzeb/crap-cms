//! Shared helper functions for the service layer.

use std::collections::HashMap;

use serde_json::Value;

use crate::{
    core::{Document, FieldDefinition, collection::Hooks},
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

/// Collect field names that are marked `admin.hidden`, including group subfields
/// with `__` prefix. Mirrors the traversal pattern of `collect_field_access_denied`.
pub(crate) fn collect_hidden_field_names(fields: &[FieldDefinition], prefix: &str) -> Vec<String> {
    use crate::core::FieldType;

    let mut hidden = Vec::new();

    for field in fields {
        let full_name = if prefix.is_empty() {
            field.name.clone()
        } else {
            format!("{}__{}", prefix, field.name)
        };

        if field.admin.hidden {
            hidden.push(full_name.clone());
            continue; // parent hidden → skip sub-fields
        }

        match field.field_type {
            FieldType::Group => {
                hidden.extend(collect_hidden_field_names(&field.fields, &full_name));
            }
            FieldType::Row | FieldType::Collapsible => {
                hidden.extend(collect_hidden_field_names(&field.fields, prefix));
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    hidden.extend(collect_hidden_field_names(&tab.fields, prefix));
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

/// Build a `PaginationResult` from query state, supporting both cursor and page modes.
///
/// Shared by `find_documents` and `search_documents` to avoid duplicating the
/// cursor/page branching logic.
pub(crate) fn build_pagination(
    docs: &[Document],
    total: i64,
    fq: &FindQuery,
    cursor_enabled: bool,
    has_timestamps: bool,
    had_cursor: bool,
    cursor_has_more: Option<bool>,
) -> query::PaginationResult {
    let limit = fq.limit.unwrap_or(total);

    if cursor_enabled {
        query::PaginationResult::builder(docs, total, limit).cursor(
            fq.order_by.as_deref(),
            has_timestamps,
            fq.before_cursor.is_some(),
            had_cursor,
            cursor_has_more,
        )
    } else {
        let offset = fq.offset.unwrap_or(0);
        let page = if limit > 0 { offset / limit + 1 } else { 1 };
        query::PaginationResult::builder(docs, total, limit).page(page, offset)
    }
}
