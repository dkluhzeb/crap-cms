//! Shared utilities for field context building: error counting, timezone handling,
//! display conditions, and template-safe naming.

use std::collections::HashMap;

use serde_json::{Map, Value, json};

use crate::{
    core::{FieldDefinition, FieldType, timezone::TIMEZONE_OPTIONS},
    hooks::{HookRunner, lifecycle::DisplayConditionResult},
};

/// Max nesting depth for recursive field context building (guard against infinite nesting).
pub const MAX_FIELD_DEPTH: usize = 5;

/// Make a template-ID-safe string from a field name (replaces `[`, `]` with `-`).
pub fn safe_template_id(name: &str) -> String {
    name.replace('[', "-").replace(']', "")
}

/// Count errors recursively in a list of field context JSON values.
/// Looks for `"error"` keys on each field, and recurses into `"sub_fields"`, `"tabs"`,
/// and `"rows"`.
pub fn count_errors_in_fields(fields: &[Value]) -> usize {
    let mut count = 0;

    for f in fields {
        if f.get("error").is_some_and(|v| !v.is_null()) {
            count += 1;
        }

        if let Some(subs) = f.get("sub_fields").and_then(|v| v.as_array()) {
            count += count_errors_in_fields(subs);
        }

        if let Some(tabs) = f.get("tabs").and_then(|v| v.as_array()) {
            for tab in tabs {
                if let Some(tab_subs) = tab.get("sub_fields").and_then(|v| v.as_array()) {
                    count += count_errors_in_fields(tab_subs);
                }
            }
        }

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

/// Collect richtext node attribute errors for a given field name.
/// Matches error keys like `{field_name}[cta#0].text` and joins messages.
pub fn collect_node_attr_errors(
    errors: &HashMap<String, String>,
    field_name: &str,
) -> Option<String> {
    let prefix = format!("{}[", field_name);

    let msgs: Vec<&str> = errors
        .iter()
        .filter(|(k, _)| k.starts_with(&prefix))
        .map(|(_, v)| v.as_str())
        .collect();

    if msgs.is_empty() {
        None
    } else {
        Some(msgs.join("; "))
    }
}

/// Split field contexts into main and sidebar based on the `position` property.
/// Returns `(main_fields, sidebar_fields)`.
pub fn split_sidebar_fields(fields: Vec<Value>) -> (Vec<Value>, Vec<Value>) {
    fields
        .into_iter()
        .partition(|f| f.get("position").and_then(|v| v.as_str()) != Some("sidebar"))
}

// ── Timezone helpers ────────────────────────────────────────────────

/// Add timezone dropdown context to a date field's template context.
///
/// Sets `timezone_enabled`, `default_timezone`, `timezone_options`, and `timezone_value`
/// on the given context when the field definition has `timezone: true`.
pub fn add_timezone_context(
    ctx: &mut Value,
    field: &FieldDefinition,
    tz_value: &str,
    config_default_tz: &str,
) {
    if !field.timezone {
        return;
    }

    let default_tz = field
        .default_timezone
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or(config_default_tz);

    ctx["timezone_enabled"] = json!(true);
    ctx["default_timezone"] = json!(default_tz);
    ctx["timezone_options"] = json!(
        TIMEZONE_OPTIONS
            .iter()
            .map(|(code, label)| json!({"value": code, "label": label}))
            .collect::<Vec<_>>()
    );
    ctx["timezone_value"] = json!(tz_value);
}

/// Inject stored timezone values into date sub-field contexts from a parent row object.
///
/// For each sub-field definition that is a date field with `timezone: true`, looks up
/// `{field_name}_tz` in the parent row and sets `timezone_value` on the corresponding context.
pub fn inject_timezone_values_from_row(
    sub_ctxs: &mut [Value],
    field_defs: &[FieldDefinition],
    parent_row: Option<&Map<String, Value>>,
) {
    let Some(row_obj) = parent_row else {
        return;
    };

    for (ctx, fd) in sub_ctxs.iter_mut().zip(field_defs.iter()) {
        if fd.field_type == FieldType::Date && fd.timezone {
            let tz_key = format!("{}_tz", fd.name);

            if let Some(tz_val) = row_obj.get(&tz_key).and_then(|v| v.as_str()) {
                ctx["timezone_value"] = json!(tz_val);
            }
        }
    }
}

/// Inject stored language picks and the picker allow-list into code sub-field
/// contexts from a parent row object.
///
/// For each sub-field definition that is a code field with a non-empty
/// `admin.languages` allow-list, looks up `{field_name}_lang` in the parent
/// row and sets `language` on the context (when present and non-empty), plus
/// emits the `languages` allow-list so the template can render the picker.
/// Mirrors the timezone pattern; both companions are stored as adjacent JSON
/// keys when the field is nested inside an array/blocks row.
pub fn inject_lang_values_from_row(
    sub_ctxs: &mut [Value],
    field_defs: &[FieldDefinition],
    parent_row: Option<&Map<String, Value>>,
) {
    let Some(row_obj) = parent_row else {
        return;
    };

    for (ctx, fd) in sub_ctxs.iter_mut().zip(field_defs.iter()) {
        if fd.field_type != FieldType::Code || fd.admin.languages.is_empty() {
            continue;
        }

        ctx["languages"] = json!(fd.admin.languages);

        let lang_key = format!("{}_lang", fd.name);
        if let Some(lang_val) = row_obj
            .get(&lang_key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            ctx["language"] = json!(lang_val);
        }
    }
}

// ── Display conditions ──────────────────────────────────────────────

/// Evaluate display conditions for field contexts and inject condition data.
/// For fields with `admin.condition`, calls the Lua function and sets:
/// - `condition_visible`: initial visibility (bool)
/// - `condition_json`: condition table for client-side evaluation (if table returned)
/// - `condition_ref`: Lua function ref for server-side evaluation (if bool returned)
pub fn apply_display_conditions(
    fields: &mut [Value],
    field_defs: &[FieldDefinition],
    form_data: &Value,
    hook_runner: &HookRunner,
    filter_hidden: bool,
) {
    let defs: Vec<&FieldDefinition> = if filter_hidden {
        field_defs.iter().filter(|f| !f.admin.hidden).collect()
    } else {
        field_defs.iter().collect()
    };

    let conditions: Vec<(&str, &Value)> = defs
        .iter()
        .filter_map(|fd| fd.admin.condition.as_deref().map(|c| (c, form_data)))
        .collect();

    if conditions.is_empty() {
        return;
    }

    let results = hook_runner.call_display_conditions_batch(&conditions);

    for (ctx, field_def) in fields.iter_mut().zip(defs.iter()) {
        apply_single_condition(ctx, field_def, &results);
    }
}

/// Apply a single display condition result to a field context.
fn apply_single_condition(
    ctx: &mut Value,
    field_def: &FieldDefinition,
    results: &HashMap<String, DisplayConditionResult>,
) {
    let Some(ref cond_ref) = field_def.admin.condition else {
        return;
    };

    let Some(result) = results.get(cond_ref.as_str()) else {
        return;
    };

    match result {
        DisplayConditionResult::Bool(visible) => {
            ctx["condition_visible"] = json!(visible);
            ctx["condition_ref"] = json!(cond_ref);
        }
        DisplayConditionResult::Table { condition, visible } => {
            ctx["condition_visible"] = json!(visible);
            ctx["condition_json"] = condition.clone();
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::core::field::{FieldAdmin, FieldDefinition, FieldType};

    use super::*;

    fn date_field_with_tz(name: &str) -> FieldDefinition {
        FieldDefinition {
            name: name.to_string(),
            field_type: FieldType::Date,
            timezone: true,
            default_timezone: Some("America/New_York".to_string()),
            ..Default::default()
        }
    }

    fn date_field_no_tz(name: &str) -> FieldDefinition {
        FieldDefinition {
            name: name.to_string(),
            field_type: FieldType::Date,
            ..Default::default()
        }
    }

    #[test]
    fn add_timezone_context_sets_options_when_enabled() {
        let field = date_field_with_tz("published_at");
        let mut ctx = json!({});

        add_timezone_context(&mut ctx, &field, "Europe/Berlin", "");

        assert_eq!(ctx["timezone_enabled"], true);
        assert_eq!(ctx["default_timezone"], "America/New_York");
        assert_eq!(ctx["timezone_value"], "Europe/Berlin");

        let options = ctx["timezone_options"].as_array().unwrap();
        assert!(!options.is_empty());
        assert_eq!(options[0]["value"], "UTC");
        assert_eq!(options[0]["label"], "UTC");
    }

    #[test]
    fn add_timezone_context_noop_when_disabled() {
        let field = date_field_no_tz("published_at");
        let mut ctx = json!({});

        add_timezone_context(&mut ctx, &field, "", "");

        assert!(ctx.get("timezone_enabled").is_none());
        assert!(ctx.get("timezone_options").is_none());
    }

    #[test]
    fn add_timezone_context_empty_default_timezone() {
        let mut field = date_field_with_tz("published_at");
        field.default_timezone = None;
        let mut ctx = json!({});

        add_timezone_context(&mut ctx, &field, "", "");

        assert_eq!(ctx["default_timezone"], "");
        assert_eq!(ctx["timezone_value"], "");
    }

    #[test]
    fn add_timezone_context_uses_config_fallback() {
        let mut field = date_field_with_tz("published_at");
        field.default_timezone = None;
        let mut ctx = json!({});

        add_timezone_context(&mut ctx, &field, "", "Europe/London");

        assert_eq!(ctx["default_timezone"], "Europe/London");
    }

    #[test]
    fn add_timezone_context_field_overrides_config() {
        let field = date_field_with_tz("published_at");
        let mut ctx = json!({});

        add_timezone_context(&mut ctx, &field, "", "Europe/London");

        assert_eq!(ctx["default_timezone"], "America/New_York");
    }

    #[test]
    fn inject_timezone_values_from_row_sets_tz_for_date_fields() {
        let field_defs = vec![date_field_with_tz("starts_at"), date_field_no_tz("ends_at")];

        let mut ctxs = vec![
            json!({"name": "items[0][starts_at]", "timezone_value": ""}),
            json!({"name": "items[0][ends_at]"}),
        ];

        let row: serde_json::Map<String, Value> = serde_json::from_value(json!({
            "starts_at": "2026-01-15",
            "starts_at_tz": "Asia/Tokyo",
            "ends_at": "2026-02-15",
        }))
        .unwrap();

        inject_timezone_values_from_row(&mut ctxs, &field_defs, Some(&row));

        assert_eq!(ctxs[0]["timezone_value"], "Asia/Tokyo");
        assert!(ctxs[1].get("timezone_value").is_none());
    }

    #[test]
    fn inject_timezone_values_from_row_noop_when_no_row() {
        let field_defs = vec![date_field_with_tz("starts_at")];
        let mut ctxs = vec![json!({"name": "items[0][starts_at]", "timezone_value": ""})];

        inject_timezone_values_from_row(&mut ctxs, &field_defs, None);

        assert_eq!(ctxs[0]["timezone_value"], "");
    }

    fn code_field_with_languages(name: &str, langs: Vec<&str>) -> FieldDefinition {
        let admin = FieldAdmin {
            languages: langs.into_iter().map(str::to_string).collect(),
            ..Default::default()
        };
        FieldDefinition {
            name: name.to_string(),
            field_type: FieldType::Code,
            admin,
            ..Default::default()
        }
    }

    #[test]
    fn inject_lang_values_from_row_sets_language_and_languages() {
        let field_defs = vec![
            code_field_with_languages("snippet", vec!["javascript", "python"]),
            code_field_with_languages("notes", vec![]), // no languages → no picker, untouched
        ];

        let mut ctxs = vec![
            json!({"name": "items[0][snippet]", "language": "javascript"}),
            json!({"name": "items[0][notes]", "language": "json"}),
        ];

        let row: serde_json::Map<String, Value> = serde_json::from_value(json!({
            "snippet": "print(1)",
            "snippet_lang": "python",
            "notes": "{}",
        }))
        .unwrap();

        inject_lang_values_from_row(&mut ctxs, &field_defs, Some(&row));

        // First field: picker enabled → languages emitted, per-row pick wins.
        assert_eq!(ctxs[0]["language"], "python");
        assert_eq!(ctxs[0]["languages"], json!(["javascript", "python"]));

        // Second field: no allow-list → context unchanged.
        assert_eq!(ctxs[1]["language"], "json");
        assert!(ctxs[1].get("languages").is_none());
    }

    #[test]
    fn inject_lang_values_from_row_keeps_default_when_lang_value_missing() {
        let field_defs = vec![code_field_with_languages(
            "snippet",
            vec!["javascript", "python"],
        )];
        let mut ctxs = vec![json!({"name": "items[0][snippet]", "language": "javascript"})];

        // Row exists but `_lang` key is absent — keep the operator default but
        // still emit `languages` so the picker renders.
        let row: serde_json::Map<String, Value> =
            serde_json::from_value(json!({"snippet": "console.log(1)"})).unwrap();

        inject_lang_values_from_row(&mut ctxs, &field_defs, Some(&row));

        assert_eq!(ctxs[0]["language"], "javascript");
        assert_eq!(ctxs[0]["languages"], json!(["javascript", "python"]));
    }
}
