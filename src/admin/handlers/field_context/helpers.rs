//! Shared utilities for field context building: error counting, timezone handling,
//! display conditions, and template-safe naming.

use std::collections::HashMap;

use serde_json::{Map, Value};

use crate::{
    admin::context::field::FieldContext,
    core::{FieldDefinition, FieldType},
    hooks::{ConditionContext, HookRunner, lifecycle::DisplayConditionResult},
};

/// Max nesting depth for recursive field context building (guard against infinite nesting).
pub const MAX_FIELD_DEPTH: usize = 5;

/// Make a template-ID-safe string from a field name (replaces `[`, `]` with `-`).
pub fn safe_template_id(name: &str) -> String {
    name.replace('[', "-").replace(']', "")
}

/// Count errors recursively across typed [`FieldContext`] sub-fields.
///
/// Walks every leaf's `base.error` and recurses through layout wrappers
/// (Group/Collapsible/Row/Tabs) and composites (Array/Blocks rows). Used
/// by the Tabs constructor to populate `tabs[*].error_count` so the UI can
/// surface validation errors hidden behind tab switches.
pub fn count_errors_in_field_contexts(fields: &[FieldContext]) -> usize {
    fields
        .iter()
        .map(|fc| {
            let mut count = usize::from(fc.base().error.is_some());

            count += match fc {
                FieldContext::Group(gf) | FieldContext::Collapsible(gf) => {
                    count_errors_in_field_contexts(&gf.sub_fields)
                }
                FieldContext::Row(rf) => count_errors_in_field_contexts(&rf.sub_fields),
                FieldContext::Tabs(tf) => tf
                    .tabs
                    .iter()
                    .map(|tp| count_errors_in_field_contexts(&tp.sub_fields))
                    .sum(),
                FieldContext::Array(af) => af.rows.as_ref().map_or(0, |rs| {
                    rs.iter()
                        .map(|r| count_errors_in_field_contexts(&r.sub_fields))
                        .sum()
                }),
                FieldContext::Blocks(bf) => bf.rows.as_ref().map_or(0, |rs| {
                    rs.iter()
                        .map(|r| count_errors_in_field_contexts(&r.sub_fields))
                        .sum()
                }),
                _ => 0,
            };

            count
        })
        .sum()
}

/// Collect richtext node attribute errors for a given field name.
/// Matches error keys like `{field_name}[cta#0].text` and joins messages.
pub fn collect_node_attr_errors(
    errors: &HashMap<String, String>,
    field_name: &str,
) -> Option<String> {
    let prefix = format!("{field_name}[");

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
pub fn split_sidebar_fields(fields: Vec<FieldContext>) -> (Vec<FieldContext>, Vec<FieldContext>) {
    fields
        .into_iter()
        .partition(|fc| fc.base().position.as_deref() != Some("sidebar"))
}

// ── Timezone helpers ────────────────────────────────────────────────

/// Inject stored timezone values into date sub-field contexts from a parent row object.
///
/// For each sub-field definition that is a date field with `timezone: true`, looks up
/// `{field_name}_tz` in the parent row and sets `timezone_value` on the corresponding context.
pub fn inject_timezone_values_from_row(
    sub_ctxs: &mut [FieldContext],
    field_defs: &[FieldDefinition],
    parent_row: Option<&Map<String, Value>>,
) {
    let Some(row_obj) = parent_row else {
        return;
    };

    for (fc, fd) in sub_ctxs.iter_mut().zip(field_defs.iter()) {
        if fd.field_type == FieldType::Date
            && fd.timezone
            && let FieldContext::Date(df) = fc
        {
            let tz_key = format!("{}_tz", fd.name);
            if let Some(tz_val) = row_obj.get(&tz_key).and_then(|v| v.as_str()) {
                df.timezone_value = Some(tz_val.to_string());
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
    sub_ctxs: &mut [FieldContext],
    field_defs: &[FieldDefinition],
    parent_row: Option<&Map<String, Value>>,
) {
    let Some(row_obj) = parent_row else {
        return;
    };

    for (fc, fd) in sub_ctxs.iter_mut().zip(field_defs.iter()) {
        if fd.field_type != FieldType::Code || fd.admin.languages.is_empty() {
            continue;
        }

        let FieldContext::Code(cf) = fc else { continue };

        cf.languages = Some(fd.admin.languages.clone());

        let lang_key = format!("{}_lang", fd.name);
        if let Some(lang_val) = row_obj
            .get(&lang_key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            cf.language = lang_val.to_string();
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
    fields: &mut [FieldContext],
    field_defs: &[FieldDefinition],
    form_data: &Value,
    hook_runner: &HookRunner,
    filter_hidden: bool,
    cond_ctx: &ConditionContext<'_>,
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

    let results = hook_runner.call_display_conditions_batch(&conditions, cond_ctx);

    for (fc, field_def) in fields.iter_mut().zip(defs.iter()) {
        apply_single_condition(fc, field_def, &results);
    }
}

/// Apply a single display condition result to a field context.
fn apply_single_condition(
    fc: &mut FieldContext,
    field_def: &FieldDefinition,
    results: &HashMap<String, DisplayConditionResult>,
) {
    let Some(ref cond_ref) = field_def.admin.condition else {
        return;
    };

    let Some(result) = results.get(cond_ref.as_str()) else {
        return;
    };

    let condition = &mut fc.base_mut().condition;

    match result {
        DisplayConditionResult::Bool(visible) => {
            condition.visible = Some(*visible);
            condition.func_ref = Some(cond_ref.clone());
        }
        DisplayConditionResult::Table {
            condition: cond,
            visible,
        } => {
            condition.visible = Some(*visible);
            condition.expr = Some(cond.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::{
        admin::context::field::{BaseFieldData, CodeField, DateField, FieldContext as FC},
        core::field::{FieldAdmin, FieldDefinition, FieldType},
    };

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

    fn date_ctx(name: &str, tz_value: Option<&str>) -> FC {
        FC::Date(DateField {
            base: BaseFieldData {
                name: name.to_string(),
                ..Default::default()
            },
            timezone_value: tz_value.map(str::to_string),
            ..Default::default()
        })
    }

    fn code_ctx(name: &str, language: &str) -> FC {
        FC::Code(CodeField {
            base: BaseFieldData {
                name: name.to_string(),
                ..Default::default()
            },
            language: language.to_string(),
            languages: None,
        })
    }

    #[test]
    fn inject_timezone_values_from_row_sets_tz_for_date_fields() {
        let field_defs = vec![date_field_with_tz("starts_at"), date_field_no_tz("ends_at")];

        let mut ctxs = vec![
            date_ctx("items[0][starts_at]", Some("")),
            date_ctx("items[0][ends_at]", None),
        ];

        let row: Map<String, Value> = serde_json::from_value(json!({
            "starts_at": "2026-01-15",
            "starts_at_tz": "Asia/Tokyo",
            "ends_at": "2026-02-15",
        }))
        .unwrap();

        inject_timezone_values_from_row(&mut ctxs, &field_defs, Some(&row));

        let FC::Date(d0) = &ctxs[0] else {
            panic!("expected date")
        };
        assert_eq!(d0.timezone_value.as_deref(), Some("Asia/Tokyo"));

        let FC::Date(d1) = &ctxs[1] else {
            panic!("expected date")
        };
        assert_eq!(d1.timezone_value, None);
    }

    #[test]
    fn inject_timezone_values_from_row_noop_when_no_row() {
        let field_defs = vec![date_field_with_tz("starts_at")];
        let mut ctxs = vec![date_ctx("items[0][starts_at]", Some(""))];

        inject_timezone_values_from_row(&mut ctxs, &field_defs, None);

        let FC::Date(d0) = &ctxs[0] else {
            panic!("expected date")
        };
        assert_eq!(d0.timezone_value.as_deref(), Some(""));
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
            code_ctx("items[0][snippet]", "javascript"),
            code_ctx("items[0][notes]", "json"),
        ];

        let row: Map<String, Value> = serde_json::from_value(json!({
            "snippet": "print(1)",
            "snippet_lang": "python",
            "notes": "{}",
        }))
        .unwrap();

        inject_lang_values_from_row(&mut ctxs, &field_defs, Some(&row));

        // First field: picker enabled → languages emitted, per-row pick wins.
        let FC::Code(c0) = &ctxs[0] else {
            panic!("expected code")
        };
        assert_eq!(c0.language, "python");
        assert_eq!(
            c0.languages.as_deref(),
            Some(&["javascript".to_string(), "python".to_string()][..])
        );

        // Second field: no allow-list → context unchanged.
        let FC::Code(c1) = &ctxs[1] else {
            panic!("expected code")
        };
        assert_eq!(c1.language, "json");
        assert!(c1.languages.is_none());
    }

    #[test]
    fn inject_lang_values_from_row_keeps_default_when_lang_value_missing() {
        let field_defs = vec![code_field_with_languages(
            "snippet",
            vec!["javascript", "python"],
        )];
        let mut ctxs = vec![code_ctx("items[0][snippet]", "javascript")];

        // Row exists but `_lang` key is absent — keep the operator default but
        // still emit `languages` so the picker renders.
        let row: Map<String, Value> =
            serde_json::from_value(json!({"snippet": "console.log(1)"})).unwrap();

        inject_lang_values_from_row(&mut ctxs, &field_defs, Some(&row));

        let FC::Code(c0) = &ctxs[0] else {
            panic!("expected code")
        };
        assert_eq!(c0.language, "javascript");
        assert_eq!(
            c0.languages.as_deref(),
            Some(&["javascript".to_string(), "python".to_string()][..])
        );
    }

    // ── safe_template_id ──────────────────────────────────────────────

    #[test]
    fn safe_template_id_simple_name() {
        assert_eq!(safe_template_id("items"), "items");
    }

    #[test]
    fn safe_template_id_with_brackets() {
        assert_eq!(safe_template_id("content[0][items]"), "content-0-items");
    }

    #[test]
    fn safe_template_id_nested_index_placeholder() {
        assert_eq!(
            safe_template_id("content[__INDEX__][items]"),
            "content-__INDEX__-items"
        );
    }

    // ── split_sidebar_fields ──────────────────────────────────────────

    use crate::admin::handlers::field_context::test_helpers::fields_from_json;

    #[test]
    fn split_sidebar_fields_separates_by_position() {
        let fields = fields_from_json(vec![
            json!({"name": "title", "field_type": "text"}),
            json!({"name": "slug", "field_type": "text", "position": "sidebar"}),
            json!({"name": "body", "field_type": "richtext"}),
            json!({"name": "status", "field_type": "select", "position": "sidebar"}),
        ]);
        let (main, sidebar) = split_sidebar_fields(fields);
        assert_eq!(main.len(), 2);
        assert_eq!(sidebar.len(), 2);
        assert_eq!(main[0].base().name, "title");
        assert_eq!(main[1].base().name, "body");
        assert_eq!(sidebar[0].base().name, "slug");
        assert_eq!(sidebar[1].base().name, "status");
    }

    #[test]
    fn split_sidebar_fields_no_sidebar() {
        let fields = fields_from_json(vec![
            json!({"name": "title", "field_type": "text"}),
            json!({"name": "body", "field_type": "richtext"}),
        ]);
        let (main, sidebar) = split_sidebar_fields(fields);
        assert_eq!(main.len(), 2);
        assert!(sidebar.is_empty());
    }

    #[test]
    fn split_sidebar_fields_all_sidebar() {
        let fields = fields_from_json(vec![
            json!({"name": "a", "field_type": "text", "position": "sidebar"}),
            json!({"name": "b", "field_type": "text", "position": "sidebar"}),
        ]);
        let (main, sidebar) = split_sidebar_fields(fields);
        assert!(main.is_empty());
        assert_eq!(sidebar.len(), 2);
    }

    #[test]
    fn split_sidebar_fields_empty() {
        let (main, sidebar) = split_sidebar_fields(vec![]);
        assert!(main.is_empty());
        assert!(sidebar.is_empty());
    }

    // ── count_errors_in_field_contexts ────────────────────────────────

    #[test]
    fn count_errors_empty_fields() {
        assert_eq!(count_errors_in_field_contexts(&[]), 0);
    }

    #[test]
    fn count_errors_no_errors() {
        let fields = fields_from_json(vec![
            json!({"field_type": "text", "name": "title", "value": "hello"}),
            json!({"field_type": "text", "name": "body", "value": "world"}),
        ]);
        assert_eq!(count_errors_in_field_contexts(&fields), 0);
    }

    #[test]
    fn count_errors_direct_errors() {
        let fields = fields_from_json(vec![
            json!({"field_type": "text", "name": "title", "error": "Required"}),
            json!({"field_type": "text", "name": "body", "value": "ok"}),
            json!({"field_type": "text", "name": "email", "error": "Invalid email"}),
        ]);
        assert_eq!(count_errors_in_field_contexts(&fields), 2);
    }

    #[test]
    fn count_errors_nested_in_sub_fields() {
        let fields = fields_from_json(vec![json!({
            "field_type": "group",
            "name": "group1",
            "sub_fields": [
                {"field_type": "text", "name": "nested1", "error": "Too short"},
                {"field_type": "text", "name": "nested2", "value": "ok"},
            ]
        })]);
        assert_eq!(count_errors_in_field_contexts(&fields), 1);
    }

    #[test]
    fn count_errors_nested_in_tabs() {
        let fields = fields_from_json(vec![json!({
            "field_type": "tabs",
            "name": "settings",
            "tabs": [
                {
                    "label": "General",
                    "sub_fields": [
                        {"field_type": "text", "name": "f1", "error": "Required"},
                        {"field_type": "text", "name": "f2", "error": "Too long"},
                    ]
                },
                {
                    "label": "Advanced",
                    "sub_fields": [
                        {"field_type": "text", "name": "f3", "value": "ok"},
                    ]
                }
            ]
        })]);
        assert_eq!(count_errors_in_field_contexts(&fields), 2);
    }

    #[test]
    fn count_errors_nested_in_array_rows() {
        let fields = fields_from_json(vec![json!({
            "field_type": "array",
            "name": "items",
            "rows": [
                {
                    "index": 0,
                    "sub_fields": [
                        {"field_type": "text", "name": "items[0][title]", "error": "Required"},
                    ]
                },
                {
                    "index": 1,
                    "sub_fields": [
                        {"field_type": "text", "name": "items[1][title]", "value": "ok"},
                    ]
                }
            ]
        })]);
        assert_eq!(count_errors_in_field_contexts(&fields), 1);
    }

    #[test]
    fn count_errors_null_error_not_counted() {
        let fields = fields_from_json(vec![
            json!({"field_type": "text", "name": "title", "error": null}),
        ]);
        assert_eq!(count_errors_in_field_contexts(&fields), 0);
    }

    // ── collect_node_attr_errors ──────────────────────────────────────

    #[test]
    fn collect_node_attr_errors_finds_matching() {
        let mut errors = HashMap::new();
        errors.insert(
            "content[cta#0].text".to_string(),
            "Text is required".to_string(),
        );
        errors.insert(
            "content[cta#0].url".to_string(),
            "URL is required".to_string(),
        );

        let result = collect_node_attr_errors(&errors, "content");
        assert!(result.is_some());
        let msg = result.unwrap();
        assert!(msg.contains("Text is required"));
        assert!(msg.contains("URL is required"));
    }

    #[test]
    fn collect_node_attr_errors_ignores_unrelated() {
        let mut errors = HashMap::new();
        errors.insert(
            "other_field[cta#0].text".to_string(),
            "Text is required".to_string(),
        );
        errors.insert("content".to_string(), "Field error".to_string());

        let result = collect_node_attr_errors(&errors, "content");
        assert!(result.is_none());
    }

    #[test]
    fn collect_node_attr_errors_empty() {
        let errors = HashMap::new();
        let result = collect_node_attr_errors(&errors, "content");
        assert!(result.is_none());
    }
}
