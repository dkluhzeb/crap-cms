//! Shared utilities for field context building: error counting, timezone handling,
//! display conditions, and template-safe naming.

use std::collections::HashMap;

use serde_json::{Map, Value};

use crate::{
    admin::context::field::FieldContext,
    core::{FieldDefinition, FieldType},
    hooks::{HookRunner, lifecycle::DisplayConditionResult},
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
            let mut count = if fc.base().error.is_some() { 1 } else { 0 };

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
                FieldContext::Array(af) => af
                    .rows
                    .as_ref()
                    .map(|rs| {
                        rs.iter()
                            .map(|r| count_errors_in_field_contexts(&r.sub_fields))
                            .sum()
                    })
                    .unwrap_or(0),
                FieldContext::Blocks(bf) => bf
                    .rows
                    .as_ref()
                    .map(|rs| {
                        rs.iter()
                            .map(|r| count_errors_in_field_contexts(&r.sub_fields))
                            .sum()
                    })
                    .unwrap_or(0),
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
            condition.condition_visible = Some(*visible);
            condition.condition_ref = Some(cond_ref.clone());
        }
        DisplayConditionResult::Table {
            condition: cond,
            visible,
        } => {
            condition.condition_visible = Some(*visible);
            condition.condition_json = Some(cond.clone());
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
}
