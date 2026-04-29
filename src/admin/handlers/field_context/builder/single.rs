//! Build a single typed [`FieldContext`] for template rendering.
//!
//! The build phase produces typed structs; the top-level
//! [`build_field_contexts`](super::build_field_contexts) entry point
//! converts to `serde_json::Value` for the enrichment phase (which is still
//! Value-based pending 1.C.2.c).

use std::collections::HashMap;

use serde_json::{Value, from_str};

use crate::{
    admin::{
        context::field::{
            ArrayField, BaseFieldData, BlockDefinition as BlockDefCtx, BlocksField, CheckboxField,
            ChoiceField, CodeField, ConditionData, DateField, FieldContext, GroupField, JoinField,
            NumberField, RelationshipField, RichtextField, RowField, TabPanel, TabsField,
            TextField, TextareaField, TimezoneOption, UploadField, ValidationAttrs,
        },
        handlers::{
            field_context::{
                MAX_FIELD_DEPTH, builder::build_select_options, collect_node_attr_errors,
                count_errors_in_field_contexts, safe_template_id,
            },
            shared::auto_label_from_name,
        },
    },
    core::{
        field::{FieldDefinition, FieldType},
        timezone::TIMEZONE_OPTIONS,
    },
    db::query::helpers::utc_to_local,
};

/// Resolve the full form name for a field, accounting for layout transparency.
fn resolve_full_name(field: &FieldDefinition, name_prefix: &str) -> String {
    if name_prefix.is_empty() {
        field.name.clone()
    } else if matches!(
        field.field_type,
        FieldType::Tabs | FieldType::Row | FieldType::Collapsible
    ) {
        name_prefix.to_string() // transparent — layout wrappers don't add their name
    } else if !name_prefix.contains('[') {
        // Top-level group chain: continue using __ naming (matches DB columns)
        format!("{}__{}", name_prefix, field.name)
    } else {
        format!("{}[{}]", name_prefix, field.name)
    }
}

/// Build the typed common base data shared by every variant. Returns
/// `(base, full_name, value)` so callers can keep using full_name and the
/// raw value string for type-specific logic.
fn build_base_field_data(
    field: &FieldDefinition,
    values: &HashMap<String, String>,
    errors: &HashMap<String, String>,
    name_prefix: &str,
    non_default_locale: bool,
) -> (BaseFieldData, String, String) {
    let full_name = resolve_full_name(field, name_prefix);
    let value_str = values.get(&full_name).cloned().unwrap_or_default();

    let label = field
        .admin
        .label
        .as_ref()
        .map(|ls| ls.resolve_default().to_string())
        .unwrap_or_else(|| auto_label_from_name(&field.name));

    let locale_locked = non_default_locale && !field.localized;

    let validation = ValidationAttrs {
        min_length: field.min_length,
        max_length: field.max_length,
        min: field.min,
        max: field.max,
        has_min: field.min.is_some().then_some(true),
        has_max: field.max.is_some().then_some(true),
    };

    let base = BaseFieldData {
        name: full_name.clone(),
        label,
        required: field.required,
        value: Value::String(value_str.clone()),
        placeholder: field
            .admin
            .placeholder
            .as_ref()
            .map(|ls| ls.resolve_default().to_string()),
        description: field
            .admin
            .description
            .as_ref()
            .map(|ls| ls.resolve_default().to_string()),
        readonly: field.admin.readonly || locale_locked,
        localized: field.localized,
        locale_locked,
        position: field.admin.position.clone(),
        error: errors.get(&full_name).cloned(),
        validation,
        condition: ConditionData::default(),
    };

    (base, full_name, value_str)
}

/// Build a typed [`FieldContext`] for a single field definition, recursing
/// into composite sub-fields.
///
/// `name_prefix`: the full form-name prefix for this field (e.g.
/// `"content[0]"` for a field inside a blocks row at index 0). Top-level
/// fields use an empty prefix.
///
/// `depth`: current nesting depth (0 = top-level). At
/// [`MAX_FIELD_DEPTH`] the recursion stops and the field is rendered as a
/// minimal text-style fallback (matches the existing behavior of bailing
/// out before type-specific dispatch).
pub fn build_single_field_context(
    field: &FieldDefinition,
    values: &HashMap<String, String>,
    errors: &HashMap<String, String>,
    name_prefix: &str,
    non_default_locale: bool,
    depth: usize,
) -> FieldContext {
    let (base, full_name, value_str) =
        build_base_field_data(field, values, errors, name_prefix, non_default_locale);

    let fc = SingleFieldCtx {
        field,
        value: &value_str,
        values,
        errors,
        name_prefix,
        full_name: &full_name,
        non_default_locale,
        depth,
    };

    construct_field_variant(base, &fc)
}

/// Common params for variant constructors.
struct SingleFieldCtx<'a> {
    field: &'a FieldDefinition,
    value: &'a str,
    values: &'a HashMap<String, String>,
    errors: &'a HashMap<String, String>,
    name_prefix: &'a str,
    full_name: &'a str,
    non_default_locale: bool,
    depth: usize,
}

/// Dispatch to the appropriate per-variant constructor. Composite variants
/// (Group/Row/Collapsible/Tabs/Array/Blocks) check `fc.depth >=
/// MAX_FIELD_DEPTH` internally and stop recursing rather than building
/// sub-fields. Non-composite variants are unaffected by depth.
fn construct_field_variant(base: BaseFieldData, fc: &SingleFieldCtx) -> FieldContext {
    match &fc.field.field_type {
        FieldType::Text if fc.field.has_many => construct_text_tags(base, fc),
        FieldType::Text => FieldContext::Text(TextField {
            base,
            has_many: None,
            tags: None,
        }),
        FieldType::Email => FieldContext::Email(TextField {
            base,
            has_many: None,
            tags: None,
        }),
        FieldType::Json => FieldContext::Json(TextField {
            base,
            has_many: None,
            tags: None,
        }),
        FieldType::Textarea => construct_textarea(base, fc),
        FieldType::Number if fc.field.has_many => construct_number_tags(base, fc),
        FieldType::Number => construct_number(base, fc),
        FieldType::Code => construct_code(base, fc),
        FieldType::Richtext => construct_richtext(base, fc),
        FieldType::Date => construct_date(base, fc),
        FieldType::Checkbox => construct_checkbox(base, fc),
        FieldType::Select => construct_choice(base, fc, FieldContext::Select),
        FieldType::Radio => construct_choice(base, fc, FieldContext::Radio),
        FieldType::Relationship => construct_relationship(base, fc),
        FieldType::Upload => construct_upload(base, fc),
        FieldType::Join => construct_join(base, fc),
        FieldType::Group => construct_group(base, fc),
        FieldType::Row => construct_row(base, fc),
        FieldType::Collapsible => construct_collapsible(base, fc),
        FieldType::Tabs => construct_tabs(base, fc),
        FieldType::Array => construct_array(base, fc),
        FieldType::Blocks => construct_blocks(base, fc),
    }
}

// ── Scalars ───────────────────────────────────────────────────────

fn construct_text_tags(mut base: BaseFieldData, fc: &SingleFieldCtx) -> FieldContext {
    let tags: Vec<String> = from_str(fc.value).unwrap_or_default();
    base.value = Value::String(tags.join(","));

    FieldContext::Text(TextField {
        base,
        has_many: Some(true),
        tags: Some(tags),
    })
}

fn construct_textarea(base: BaseFieldData, fc: &SingleFieldCtx) -> FieldContext {
    FieldContext::Textarea(TextareaField {
        base,
        rows: fc.field.admin.rows.unwrap_or(8),
        resizable: fc.field.admin.resizable,
    })
}

fn construct_number(base: BaseFieldData, fc: &SingleFieldCtx) -> FieldContext {
    FieldContext::Number(NumberField {
        base,
        step: fc.field.admin.step.as_deref().unwrap_or("any").to_string(),
        has_many: None,
        tags: None,
    })
}

fn construct_number_tags(mut base: BaseFieldData, fc: &SingleFieldCtx) -> FieldContext {
    let tags: Vec<String> = from_str(fc.value).unwrap_or_default();
    base.value = Value::String(tags.join(","));

    FieldContext::Number(NumberField {
        base,
        step: fc.field.admin.step.as_deref().unwrap_or("any").to_string(),
        has_many: Some(true),
        tags: Some(tags),
    })
}

fn construct_code(base: BaseFieldData, fc: &SingleFieldCtx) -> FieldContext {
    let default_lang = fc.field.admin.language.as_deref().unwrap_or("json");
    let chosen = fc
        .values
        .get(&format!("{}_lang", fc.full_name))
        .map(String::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or(default_lang);

    let languages = if !fc.field.admin.languages.is_empty() {
        Some(fc.field.admin.languages.clone())
    } else {
        None
    };

    FieldContext::Code(CodeField {
        base,
        language: chosen.to_string(),
        languages,
    })
}

fn construct_richtext(mut base: BaseFieldData, fc: &SingleFieldCtx) -> FieldContext {
    let resizable = fc.field.admin.resizable;
    let features = if !fc.field.admin.features.is_empty() {
        Some(fc.field.admin.features.clone())
    } else {
        None
    };
    let richtext_format = fc
        .field
        .admin
        .richtext_format
        .as_deref()
        .unwrap_or("html")
        .to_string();
    let node_names = if !fc.field.admin.nodes.is_empty() {
        Some(fc.field.admin.nodes.clone())
    } else {
        None
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

fn construct_date(base: BaseFieldData, fc: &SingleFieldCtx) -> FieldContext {
    let appearance = fc
        .field
        .picker_appearance
        .as_deref()
        .unwrap_or("dayOnly")
        .to_string();

    let tz_key = format!("{}_tz", fc.full_name);
    let tz_value = fc
        .values
        .get(&tz_key)
        .map(|s| s.as_str())
        .unwrap_or("")
        .trim();

    let display_value = if !tz_value.is_empty() && !fc.value.is_empty() {
        utc_to_local(fc.value, tz_value).unwrap_or_else(|| fc.value.to_string())
    } else {
        fc.value.to_string()
    };

    let (date_only_value, datetime_local_value) = match appearance.as_str() {
        "dayOnly" => (
            Some(
                display_value
                    .get(..10)
                    .unwrap_or(&display_value)
                    .to_string(),
            ),
            None,
        ),
        "dayAndTime" => (
            None,
            Some(
                display_value
                    .get(..16)
                    .unwrap_or(&display_value)
                    .to_string(),
            ),
        ),
        _ => (None, None),
    };

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
        min_date: fc.field.min_date.clone(),
        max_date: fc.field.max_date.clone(),
        timezone_enabled,
        default_timezone,
        timezone_options,
        timezone_value,
    })
}

fn construct_checkbox(base: BaseFieldData, fc: &SingleFieldCtx) -> FieldContext {
    FieldContext::Checkbox(CheckboxField {
        base,
        checked: matches!(fc.value, "1" | "true" | "on" | "yes"),
    })
}

fn construct_choice<F>(base: BaseFieldData, fc: &SingleFieldCtx, variant: F) -> FieldContext
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

// ── References ────────────────────────────────────────────────────

fn construct_relationship(base: BaseFieldData, fc: &SingleFieldCtx) -> FieldContext {
    let (relationship_collection, has_many, polymorphic, collections) =
        if let Some(ref rc) = fc.field.relationship {
            let (poly_flag, poly_list) = if rc.is_polymorphic() {
                (
                    Some(true),
                    Some(rc.polymorphic.iter().map(ToString::to_string).collect()),
                )
            } else {
                (None, None)
            };
            (
                Some(rc.collection.to_string()),
                Some(rc.has_many),
                poly_flag,
                poly_list,
            )
        } else {
            (None, None, None, None)
        };

    let picker = fc.field.admin.picker.clone();

    FieldContext::Relationship(RelationshipField {
        base,
        relationship_collection,
        has_many,
        polymorphic,
        collections,
        picker,
        selected_items: None,
    })
}

fn construct_upload(base: BaseFieldData, fc: &SingleFieldCtx) -> FieldContext {
    let (relationship_collection, has_many) = if let Some(ref rc) = fc.field.relationship {
        let hm = if rc.has_many { Some(true) } else { None };
        (Some(rc.collection.to_string()), hm)
    } else {
        (None, None)
    };

    let picker_str = fc.field.admin.picker.as_deref().unwrap_or("drawer");
    let picker = if picker_str == "none" {
        None
    } else {
        Some(picker_str.to_string())
    };

    FieldContext::Upload(UploadField {
        base,
        relationship_collection,
        has_many,
        picker,
        selected_items: None,
        selected_filename: None,
        selected_preview_url: None,
    })
}

fn construct_join(mut base: BaseFieldData, fc: &SingleFieldCtx) -> FieldContext {
    base.readonly = true;

    let (join_collection, join_on) = if let Some(ref jc) = fc.field.join {
        (Some(jc.collection.to_string()), Some(jc.on.clone()))
    } else {
        (None, None)
    };

    FieldContext::Join(JoinField {
        base,
        join_collection,
        join_on,
        join_items: None,
        join_count: None,
    })
}

// ── Composites ────────────────────────────────────────────────────

/// Build sub-fields for layout wrappers (Row, Collapsible, Tabs).
/// Top-level wrappers use empty prefix, nested ones use the full_name.
fn build_layout_sub_fields(
    fields: &[FieldDefinition],
    values: &HashMap<String, String>,
    errors: &HashMap<String, String>,
    name_prefix: &str,
    full_name: &str,
    non_default_locale: bool,
    depth: usize,
) -> Vec<FieldContext> {
    let prefix = if name_prefix.is_empty() {
        ""
    } else {
        full_name
    };

    fields
        .iter()
        .map(|sf| {
            build_single_field_context(sf, values, errors, prefix, non_default_locale, depth + 1)
        })
        .collect()
}

fn construct_group(base: BaseFieldData, fc: &SingleFieldCtx) -> FieldContext {
    let sub_fields = if fc.depth >= MAX_FIELD_DEPTH {
        Vec::new()
    } else {
        let prefix = if fc.name_prefix.is_empty() {
            fc.field.name.clone()
        } else {
            fc.full_name.to_string()
        };

        let child_non_default_locale = if fc.field.localized {
            false
        } else {
            fc.non_default_locale
        };

        fc.field
            .fields
            .iter()
            .map(|sf| {
                build_single_field_context(
                    sf,
                    fc.values,
                    fc.errors,
                    &prefix,
                    child_non_default_locale,
                    fc.depth + 1,
                )
            })
            .collect()
    };

    FieldContext::Group(GroupField {
        base,
        sub_fields,
        collapsed: fc.field.admin.collapsed,
    })
}

fn construct_row(base: BaseFieldData, fc: &SingleFieldCtx) -> FieldContext {
    let sub_fields = if fc.depth >= MAX_FIELD_DEPTH {
        Vec::new()
    } else {
        build_layout_sub_fields(
            &fc.field.fields,
            fc.values,
            fc.errors,
            fc.name_prefix,
            fc.full_name,
            fc.non_default_locale,
            fc.depth,
        )
    };

    FieldContext::Row(RowField { base, sub_fields })
}

fn construct_collapsible(base: BaseFieldData, fc: &SingleFieldCtx) -> FieldContext {
    let sub_fields = if fc.depth >= MAX_FIELD_DEPTH {
        Vec::new()
    } else {
        build_layout_sub_fields(
            &fc.field.fields,
            fc.values,
            fc.errors,
            fc.name_prefix,
            fc.full_name,
            fc.non_default_locale,
            fc.depth,
        )
    };

    FieldContext::Collapsible(GroupField {
        base,
        sub_fields,
        collapsed: fc.field.admin.collapsed,
    })
}

fn construct_tabs(base: BaseFieldData, fc: &SingleFieldCtx) -> FieldContext {
    let tabs: Vec<TabPanel> = if fc.depth >= MAX_FIELD_DEPTH {
        Vec::new()
    } else {
        fc.field
            .tabs
            .iter()
            .map(|tab| {
                let sub_fields = build_layout_sub_fields(
                    &tab.fields,
                    fc.values,
                    fc.errors,
                    fc.name_prefix,
                    fc.full_name,
                    fc.non_default_locale,
                    fc.depth,
                );

                let error_count = count_errors_in_field_contexts(&sub_fields);
                let error_count_opt = if error_count > 0 {
                    Some(error_count)
                } else {
                    None
                };

                TabPanel {
                    label: tab.label.clone(),
                    sub_fields,
                    error_count: error_count_opt,
                    description: tab.description.clone(),
                }
            })
            .collect()
    };

    FieldContext::Tabs(TabsField { base, tabs })
}

fn construct_array(base: BaseFieldData, fc: &SingleFieldCtx) -> FieldContext {
    let template_prefix = format!("{}[__INDEX__]", fc.full_name);
    let sub_fields: Vec<FieldContext> = if fc.depth >= MAX_FIELD_DEPTH {
        Vec::new()
    } else {
        fc.field
            .fields
            .iter()
            .map(|sf| {
                build_single_field_context(
                    sf,
                    &HashMap::new(),
                    &HashMap::new(),
                    &template_prefix,
                    fc.non_default_locale,
                    fc.depth + 1,
                )
            })
            .collect()
    };

    FieldContext::Array(ArrayField {
        base,
        sub_fields,
        rows: None,
        row_count: 0,
        template_id: safe_template_id(fc.full_name),
        min_rows: fc.field.min_rows,
        max_rows: fc.field.max_rows,
        init_collapsed: fc.field.admin.collapsed,
        add_label: fc
            .field
            .admin
            .labels_singular
            .as_ref()
            .map(|ls| ls.resolve_default().to_string()),
        label_field: fc.field.admin.label_field.clone(),
    })
}

fn construct_blocks(base: BaseFieldData, fc: &SingleFieldCtx) -> FieldContext {
    let template_prefix = format!("{}[__INDEX__]", fc.full_name);

    let block_definitions: Vec<BlockDefCtx> = if fc.depth >= MAX_FIELD_DEPTH {
        Vec::new()
    } else {
        fc.field
            .blocks
            .iter()
            .map(|bd| {
                let fields: Vec<FieldContext> = bd
                    .fields
                    .iter()
                    .map(|sf| {
                        build_single_field_context(
                            sf,
                            &HashMap::new(),
                            &HashMap::new(),
                            &template_prefix,
                            fc.non_default_locale,
                            fc.depth + 1,
                        )
                    })
                    .collect();

                let label = bd
                    .label
                    .as_ref()
                    .map(|ls| ls.resolve_default().to_string())
                    .unwrap_or_else(|| bd.block_type.clone());

                BlockDefCtx {
                    block_type: bd.block_type.clone(),
                    label,
                    fields,
                    label_field: bd.label_field.clone(),
                    group: bd.group.clone(),
                    image_url: bd.image_url.clone(),
                }
            })
            .collect()
    };

    FieldContext::Blocks(BlocksField {
        base,
        block_definitions,
        rows: None,
        row_count: 0,
        template_id: safe_template_id(fc.full_name),
        min_rows: fc.field.min_rows,
        max_rows: fc.field.max_rows,
        init_collapsed: fc.field.admin.collapsed,
        add_label: fc
            .field
            .admin
            .labels_singular
            .as_ref()
            .map(|ls| ls.resolve_default().to_string()),
        picker: fc.field.admin.picker.clone(),
        label_field: fc.field.admin.label_field.clone(),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::core::field::{FieldDefinition, FieldType};

    use super::build_single_field_context;

    fn group_field(name: &str, localized: bool, children: Vec<FieldDefinition>) -> FieldDefinition {
        FieldDefinition {
            name: name.to_string(),
            field_type: FieldType::Group,
            localized,
            fields: children,
            ..Default::default()
        }
    }

    fn text_field(name: &str) -> FieldDefinition {
        FieldDefinition {
            name: name.to_string(),
            field_type: FieldType::Text,
            ..Default::default()
        }
    }

    #[test]
    fn non_localized_group_in_non_default_locale_locks_children() {
        let field = group_field("meta", false, vec![text_field("title")]);
        let values = HashMap::new();
        let errors = HashMap::new();

        let ctx = build_single_field_context(&field, &values, &errors, "", true, 0).to_value();

        // The group itself should be locale-locked
        assert_eq!(ctx["locale_locked"], true);

        // Children should inherit locale lock (non_default_locale=true, group not localized)
        let sub = &ctx["sub_fields"][0];
        assert_eq!(
            sub["locale_locked"], true,
            "child of non-localized group must be locale_locked in non-default locale"
        );
        assert_eq!(sub["readonly"], true);
    }

    #[test]
    fn localized_group_in_non_default_locale_unlocks_children() {
        let field = group_field("meta", true, vec![text_field("title")]);
        let values = HashMap::new();
        let errors = HashMap::new();

        let ctx = build_single_field_context(&field, &values, &errors, "", true, 0).to_value();

        // The localized group itself should NOT be locale-locked
        assert_eq!(ctx["locale_locked"], false);

        // Children should be editable (non_default_locale reset to false for localized group)
        let sub = &ctx["sub_fields"][0];
        assert_eq!(
            sub["locale_locked"], false,
            "child of localized group must NOT be locale_locked"
        );
        assert_eq!(sub["readonly"], false);
    }

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

        let ctx = build_single_field_context(&field, &values, &errors, "", false, 0).to_value();
        assert_eq!(ctx["language"], "javascript");
    }

    #[test]
    fn code_field_defaults_to_json_when_unconfigured() {
        let field = code_field("snippet", None);
        let values = HashMap::new();
        let errors = HashMap::new();

        let ctx = build_single_field_context(&field, &values, &errors, "", false, 0).to_value();
        assert_eq!(ctx["language"], "json");
    }

    /// Regression test for the bug: a Code sub-field inside a `blocks` field
    /// previously rendered with `data-language=""` (then JS fallback to "json")
    /// even when `admin.language = "javascript"` was configured. The fix added
    /// `FieldType::Code` to the dispatch so the `<template>` rendering of
    /// block sub-fields picks up the language too.
    #[test]
    fn code_subfield_inside_blocks_carries_language() {
        use crate::core::field::BlockDefinition;

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
        let ctx =
            build_single_field_context(&blocks_field, &values, &errors, "", false, 0).to_value();

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

        let ctx = build_single_field_context(&field, &values, &errors, "", false, 0).to_value();
        assert_eq!(ctx["language"], "javascript");
        assert_eq!(
            ctx["languages"],
            serde_json::json!(["javascript", "python"])
        );
    }

    #[test]
    fn code_field_omits_languages_when_picker_not_configured() {
        let field = code_field("snippet", Some("javascript"));
        let values = HashMap::new();
        let errors = HashMap::new();

        let ctx = build_single_field_context(&field, &values, &errors, "", false, 0).to_value();
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

        let ctx = build_single_field_context(&field, &values, &errors, "", false, 0).to_value();
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

        let ctx = build_single_field_context(&field, &values, &errors, "", false, 0).to_value();
        assert_eq!(ctx["language"], "javascript");
    }

    /// Mirrors the projects example: a code field inside a `code_block`
    /// block-definition with `admin.languages` set. The picker MUST show up
    /// (data-languages attribute and hidden _lang input both rely on
    /// ctx["languages"]) — verify the block-template rendering carries it.
    #[test]
    fn code_subfield_inside_blocks_carries_languages_allowlist() {
        use crate::core::field::BlockDefinition;

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
        let ctx =
            build_single_field_context(&blocks_field, &values, &errors, "", false, 0).to_value();

        let sub_field = &ctx["block_definitions"][0]["fields"][0];
        assert_eq!(sub_field["language"], "javascript");
        assert_eq!(
            sub_field["languages"],
            serde_json::json!(["javascript", "python", "html"]),
            "block-template code field must carry the picker allow-list so the rendered \
             <crap-code> gets data-languages and the hidden _lang input shows up"
        );
    }
}
