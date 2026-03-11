//! Field context builders for admin form rendering.
//! Builds template context objects from field definitions, handling recursive
//! composite types (Array, Blocks, Group) with nesting depth limits.

mod builder;
mod enrich;

// Re-export public API so external imports (e.g. `super::field_context::build_field_contexts`)
// continue to work unchanged.
pub(super) use builder::build_field_contexts;
pub(super) use enrich::enrich_field_contexts;

/// Make a template-ID-safe string from a field name (replaces `[`, `]` with `-`).
pub(super) fn safe_template_id(name: &str) -> String {
    name.replace('[', "-").replace(']', "")
}

/// Count errors recursively in a list of field context JSON values.
/// Looks for `"error"` keys on each field, and recurses into `"sub_fields"` and `"tabs"`.
pub(super) fn count_errors_in_fields(fields: &[serde_json::Value]) -> usize {
    let mut count = 0;
    for f in fields {
        if f.get("error").is_some_and(|v| !v.is_null()) {
            count += 1;
        }
        // Recurse into sub_fields (Group, Row, Collapsible)
        if let Some(subs) = f.get("sub_fields").and_then(|v| v.as_array()) {
            count += count_errors_in_fields(subs);
        }
        // Recurse into tabs
        if let Some(tabs) = f.get("tabs").and_then(|v| v.as_array()) {
            for tab in tabs {
                if let Some(tab_subs) = tab.get("sub_fields").and_then(|v| v.as_array()) {
                    count += count_errors_in_fields(tab_subs);
                }
            }
        }
        // Recurse into array rows
        if let Some(rows) = f.get("rows").and_then(|v| v.as_array()) {
            for row in rows {
                if let Some(row_subs) = row.get("sub_fields").and_then(|v| v.as_array()) {
                    count += count_errors_in_fields(row_subs);
                }
            }
        }
    }
    count
}

/// Max nesting depth for recursive field context building (guard against infinite nesting).
pub(super) const MAX_FIELD_DEPTH: usize = 5;

/// Evaluate display conditions for field contexts and inject condition data.
/// For fields with `admin.condition`, calls the Lua function and sets:
/// - `condition_visible`: initial visibility (bool)
/// - `condition_json`: condition table for client-side evaluation (if table returned)
/// - `condition_ref`: Lua function ref for server-side evaluation (if bool returned)
pub(super) fn apply_display_conditions(
    fields: &mut [serde_json::Value],
    field_defs: &[crate::core::field::FieldDefinition],
    form_data: &serde_json::Value,
    hook_runner: &crate::hooks::lifecycle::HookRunner,
    filter_hidden: bool,
) {
    use crate::hooks::lifecycle::DisplayConditionResult;

    let defs: Vec<&crate::core::field::FieldDefinition> = if filter_hidden {
        field_defs.iter().filter(|f| !f.admin.hidden).collect()
    } else {
        field_defs.iter().collect()
    };

    // Collect all conditions that need evaluation
    let conditions: Vec<(&str, &serde_json::Value)> = defs
        .iter()
        .filter_map(|fd| fd.admin.condition.as_deref().map(|c| (c, form_data)))
        .collect();

    if conditions.is_empty() {
        return;
    }

    // Evaluate all conditions in a single VM acquisition
    let results = hook_runner.call_display_conditions_batch(&conditions);

    for (ctx, field_def) in fields.iter_mut().zip(defs.iter()) {
        if let Some(ref cond_ref) = field_def.admin.condition {
            if let Some(result) = results.get(cond_ref.as_str()) {
                match result {
                    DisplayConditionResult::Bool(visible) => {
                        ctx["condition_visible"] = serde_json::json!(visible);
                        ctx["condition_ref"] = serde_json::json!(cond_ref);
                    }
                    DisplayConditionResult::Table { condition, visible } => {
                        ctx["condition_visible"] = serde_json::json!(visible);
                        ctx["condition_json"] = condition.clone();
                    }
                }
            }
        }
    }
}

/// Split field contexts into main and sidebar based on the `position` property.
/// Returns `(main_fields, sidebar_fields)`.
pub(super) fn split_sidebar_fields(
    fields: Vec<serde_json::Value>,
) -> (Vec<serde_json::Value>, Vec<serde_json::Value>) {
    fields
        .into_iter()
        .partition(|f| f.get("position").and_then(|v| v.as_str()) != Some("sidebar"))
}
