//! Per-variant constructors for scalar fields (text, number, code,
//! richtext, JSON, date, checkbox, select/radio).

use serde_json::Value;

use crate::{
    admin::{
        context::field::{
            BaseFieldData, CheckboxField, ChoiceField, CodeField, DateField, FieldContext,
            JsonField, NumberField, RichtextField, TextField, TextareaField, TimezoneOption,
        },
        handlers::field_context::{
            builder::{build_select_options, single::entry::SingleFieldCtx},
            collect_node_attr_errors, date_picker_values, json_textarea_value, picker_step,
            tag_values, tags_input_value,
        },
    },
    core::{PickerAppearance, parse_truthy, timezone::TIMEZONE_OPTIONS},
    db::query::helpers::{lang_column, tz_column},
};

pub(super) fn construct_text_tags(mut base: BaseFieldData, fc: &SingleFieldCtx) -> FieldContext {
    let tags = tag_values(fc.value);
    base.value = tags_input_value(&tags);

    FieldContext::Text(TextField {
        base,
        has_many: Some(true),
        tags: Some(tags),
    })
}

pub(super) fn construct_textarea(base: BaseFieldData, fc: &SingleFieldCtx) -> FieldContext {
    FieldContext::Textarea(TextareaField {
        base,
        rows: fc.field.admin.rows.unwrap_or(8),
        resizable: fc.field.admin.resizable,
    })
}

/// Default HTML `step` for a number field: `1` when restricted to whole
/// numbers (`integer = true`), else `any`. An explicit `admin.step` wins.
fn number_step(fc: &SingleFieldCtx) -> String {
    let default = if fc.field.integer { "1" } else { "any" };
    fc.field
        .admin
        .step
        .as_deref()
        .unwrap_or(default)
        .to_string()
}

pub(super) fn construct_number(base: BaseFieldData, fc: &SingleFieldCtx) -> FieldContext {
    FieldContext::Number(NumberField {
        base,
        step: number_step(fc),
        has_many: None,
        tags: None,
    })
}

pub(super) fn construct_number_tags(mut base: BaseFieldData, fc: &SingleFieldCtx) -> FieldContext {
    let tags = tag_values(fc.value);
    base.value = tags_input_value(&tags);

    FieldContext::Number(NumberField {
        base,
        step: number_step(fc),
        has_many: Some(true),
        tags: Some(tags),
    })
}

pub(super) fn construct_code(base: BaseFieldData, fc: &SingleFieldCtx) -> FieldContext {
    let default_lang = fc.field.admin.language.as_deref().unwrap_or("json");
    let chosen = fc
        .values
        .get(&lang_column(fc.full_name))
        .map(String::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or(default_lang);

    let languages = if fc.field.admin.languages.is_empty() {
        None
    } else {
        Some(fc.field.admin.languages.clone())
    };

    FieldContext::Code(CodeField {
        base,
        language: chosen.to_string(),
        languages,
        rows: fc.field.admin.rows,
    })
}

pub(super) fn construct_richtext(mut base: BaseFieldData, fc: &SingleFieldCtx) -> FieldContext {
    let resizable = fc.field.admin.resizable;
    let features = if fc.field.admin.features.is_empty() {
        None
    } else {
        Some(fc.field.admin.features.clone())
    };
    let richtext_format = fc
        .field
        .admin
        .richtext_format
        .as_deref()
        .unwrap_or("html")
        .to_string();
    let node_names = if fc.field.admin.nodes.is_empty() {
        None
    } else {
        Some(fc.field.admin.nodes.clone())
    };

    // Node-attribute errors fall back when there's no direct error for the
    // field. Mirrors the old `single_richtext` behavior.
    if base.error.is_none()
        && let Some(node_err) = collect_node_attr_errors(fc.errors, fc.full_name)
    {
        base.error = Some(node_err);
    }

    FieldContext::Richtext(RichtextField {
        base,
        resizable,
        richtext_format,
        features,
        node_names,
        custom_nodes: None,
    })
}

/// The JSON textarea shows a stored value pretty-printed; the text it submits
/// is parsed back on write, so a no-op save stores the same value.
pub(super) fn construct_json(mut base: BaseFieldData, fc: &SingleFieldCtx) -> FieldContext {
    base.value = Value::String(json_textarea_value(fc.value));

    FieldContext::Json(JsonField::new(base, fc.field.admin.rows))
}

pub(super) fn construct_date(base: BaseFieldData, fc: &SingleFieldCtx) -> FieldContext {
    let appearance = fc
        .field
        .picker_appearance
        .as_ref()
        .map_or("dayOnly", PickerAppearance::as_str)
        .to_string();

    let tz_key = tz_column(fc.full_name);
    let tz_value = fc
        .values
        .get(&tz_key)
        .map_or("", std::string::String::as_str)
        .trim();

    let (date_only_value, datetime_local_value) =
        date_picker_values(fc.value, tz_value, &appearance);
    let step = picker_step(fc.value, tz_value, &appearance);

    let (timezone_enabled, default_timezone, timezone_options, timezone_value) =
        if fc.field.timezone {
            let default_tz = fc
                .field
                .default_timezone
                .as_deref()
                .filter(|s| !s.is_empty())
                .unwrap_or("");
            let options: Vec<TimezoneOption> = TIMEZONE_OPTIONS
                .iter()
                .map(|(code, label)| TimezoneOption {
                    value: (*code).to_string(),
                    label: (*label).to_string(),
                })
                .collect();

            (
                Some(true),
                Some(default_tz.to_string()),
                Some(options),
                Some(tz_value.to_string()),
            )
        } else {
            (None, None, None, None)
        };

    FieldContext::Date(DateField {
        base,
        picker_appearance: appearance,
        date_only_value,
        datetime_local_value,
        step,
        min_date: fc.field.min_date.clone(),
        max_date: fc.field.max_date.clone(),
        timezone_enabled,
        default_timezone,
        timezone_options,
        timezone_value,
    })
}

pub(super) fn construct_checkbox(base: BaseFieldData, fc: &SingleFieldCtx) -> FieldContext {
    // A new-item form has no submitted or stored value; fall back to the field's
    // boolean `default_value` so a `default_value = true` checkbox renders
    // checked (and, left as-is, submits `"on"`). A present value — an existing
    // row's stored `0`/`1`, or a re-rendered submission — takes precedence.
    let checked = if fc.value.is_empty() {
        fc.field
            .default_value
            .as_ref()
            .and_then(Value::as_bool)
            .unwrap_or(false)
    } else {
        parse_truthy(fc.value)
    };

    FieldContext::Checkbox(CheckboxField { base, checked })
}

pub(super) fn construct_choice<F>(
    base: BaseFieldData,
    fc: &SingleFieldCtx,
    variant: F,
) -> FieldContext
where
    F: FnOnce(ChoiceField) -> FieldContext,
{
    let (options, has_many_flag) = build_select_options(fc.field, fc.value);
    let has_many = if has_many_flag { Some(true) } else { None };

    variant(ChoiceField {
        base,
        options,
        has_many,
    })
}

#[cfg(test)]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::case_sensitive_file_extension_comparisons,
    clippy::items_after_statements,
    clippy::match_wildcard_for_single_variants,
    clippy::missing_panics_doc,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::unreadable_literal,
    clippy::used_underscore_binding
)]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;

    use crate::admin::handlers::field_context::builder::build_single_field_context;
    use crate::core::{BlockDefinition, FieldDefinition, FieldType};

    fn code_field(name: &str, language: Option<&str>) -> FieldDefinition {
        let mut f = FieldDefinition {
            name: name.to_string(),
            field_type: FieldType::Code,
            ..Default::default()
        };
        f.admin.language = language.map(str::to_string);
        f
    }

    #[test]
    fn code_field_carries_language_attr() {
        let field = code_field("snippet", Some("javascript"));
        let values = HashMap::new();
        let errors = HashMap::new();

        let ctx =
            build_single_field_context(&field, &values, &errors, "", false, false, 0).to_value();
        assert_eq!(ctx["language"], "javascript");
    }

    #[test]
    fn code_field_defaults_to_json_when_unconfigured() {
        let field = code_field("snippet", None);
        let values = HashMap::new();
        let errors = HashMap::new();

        let ctx =
            build_single_field_context(&field, &values, &errors, "", false, false, 0).to_value();
        assert_eq!(ctx["language"], "json");
    }

    /// Regression test for the bug: a Code sub-field inside a `blocks` field
    /// previously rendered with `data-language=""` (then JS fallback to "json")
    /// even when `admin.language = "javascript"` was configured. The fix added
    /// `FieldType::Code` to the dispatch so the `<template>` rendering of
    /// block sub-fields picks up the language too.
    #[test]
    fn code_subfield_inside_blocks_carries_language() {
        let code = code_field("snippet", Some("javascript"));
        let block = BlockDefinition {
            block_type: "code_block".to_string(),
            label: None,
            label_field: None,
            group: None,
            image_url: None,
            fields: vec![code],
        };
        let blocks_field = FieldDefinition {
            name: "content".to_string(),
            field_type: FieldType::Blocks,
            blocks: vec![block],
            ..Default::default()
        };

        let values = HashMap::new();
        let errors = HashMap::new();
        let ctx = build_single_field_context(&blocks_field, &values, &errors, "", false, false, 0)
            .to_value();

        let sub_field = &ctx["block_definitions"][0]["fields"][0];
        assert_eq!(
            sub_field["language"], "javascript",
            "code sub-field inside a blocks template must carry the configured language"
        );
    }

    fn code_field_with_languages(
        name: &str,
        default_language: &str,
        languages: Vec<&str>,
    ) -> FieldDefinition {
        let mut f = code_field(name, Some(default_language));
        f.admin.languages = languages.into_iter().map(str::to_string).collect();
        f
    }

    #[test]
    fn code_field_emits_languages_when_picker_configured() {
        let field =
            code_field_with_languages("snippet", "javascript", vec!["javascript", "python"]);
        let values = HashMap::new();
        let errors = HashMap::new();

        let ctx =
            build_single_field_context(&field, &values, &errors, "", false, false, 0).to_value();
        assert_eq!(ctx["language"], "javascript");
        assert_eq!(ctx["languages"], json!(["javascript", "python"]));
    }

    #[test]
    fn code_field_omits_languages_when_picker_not_configured() {
        let field = code_field("snippet", Some("javascript"));
        let values = HashMap::new();
        let errors = HashMap::new();

        let ctx =
            build_single_field_context(&field, &values, &errors, "", false, false, 0).to_value();
        // No `languages` key when the operator hasn't opted into the picker.
        assert!(ctx.get("languages").is_none());
    }

    #[test]
    fn code_field_uses_per_document_lang_value_when_set() {
        let field =
            code_field_with_languages("snippet", "javascript", vec!["javascript", "python"]);
        let mut values = HashMap::new();
        // Editor previously chose "python" — companion column value is in the
        // values map keyed by `<full_name>_lang`.
        values.insert("snippet_lang".to_string(), "python".to_string());
        let errors = HashMap::new();

        let ctx =
            build_single_field_context(&field, &values, &errors, "", false, false, 0).to_value();
        assert_eq!(
            ctx["language"], "python",
            "per-document _lang value should win over the operator default"
        );
    }

    #[test]
    fn code_field_falls_back_to_default_when_lang_value_empty() {
        let field =
            code_field_with_languages("snippet", "javascript", vec!["javascript", "python"]);
        let mut values = HashMap::new();
        values.insert("snippet_lang".to_string(), String::new());
        let errors = HashMap::new();

        let ctx =
            build_single_field_context(&field, &values, &errors, "", false, false, 0).to_value();
        assert_eq!(ctx["language"], "javascript");
    }

    /// Mirrors the projects example: a code field inside a `code_block`
    /// block-definition with `admin.languages` set. The picker MUST show up
    /// (data-languages attribute and hidden `_lang` input both rely on
    /// `ctx["languages"]`) — verify the block-template rendering carries it.
    #[test]
    fn code_subfield_inside_blocks_carries_languages_allowlist() {
        let code =
            code_field_with_languages("code", "javascript", vec!["javascript", "python", "html"]);
        let block = BlockDefinition {
            block_type: "code_block".to_string(),
            label: None,
            label_field: None,
            group: None,
            image_url: None,
            fields: vec![code],
        };
        let blocks_field = FieldDefinition {
            name: "content".to_string(),
            field_type: FieldType::Blocks,
            blocks: vec![block],
            ..Default::default()
        };

        let values = HashMap::new();
        let errors = HashMap::new();
        let ctx = build_single_field_context(&blocks_field, &values, &errors, "", false, false, 0)
            .to_value();

        let sub_field = &ctx["block_definitions"][0]["fields"][0];
        assert_eq!(sub_field["language"], "javascript");
        assert_eq!(
            sub_field["languages"],
            json!(["javascript", "python", "html"]),
            "block-template code field must carry the picker allow-list so the rendered \
             <crap-code> gets data-languages and the hidden _lang input shows up"
        );
    }
}
