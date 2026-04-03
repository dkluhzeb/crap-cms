//! Build a single field context for template rendering.

use std::collections::HashMap;

use serde_json::{Value, from_str, json};

use crate::{
    admin::handlers::{
        field_context::{
            MAX_FIELD_DEPTH, add_timezone_context, builder::build_select_options,
            collect_node_attr_errors, count_errors_in_fields,
        },
        shared::auto_label_from_name,
    },
    core::field::{FieldDefinition, FieldType},
    db::query::helpers::utc_to_local,
};

use super::field_type_extras::{apply_row_props, apply_validation_props};

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

/// Build base field context with common properties and validation attributes.
/// Returns (ctx, full_name, value).
fn build_base_field_context(
    field: &FieldDefinition,
    values: &HashMap<String, String>,
    errors: &HashMap<String, String>,
    name_prefix: &str,
    non_default_locale: bool,
) -> (Value, String, String) {
    let full_name = resolve_full_name(field, name_prefix);
    let value = values.get(&full_name).cloned().unwrap_or_default();

    let label = field
        .admin
        .label
        .as_ref()
        .map(|ls| ls.resolve_default().to_string())
        .unwrap_or_else(|| auto_label_from_name(&field.name));

    let locale_locked = non_default_locale && !field.localized;

    let mut ctx = json!({
        "name": full_name,
        "field_type": field.field_type.as_str(),
        "label": label,
        "required": field.required,
        "value": value,
        "placeholder": field.admin.placeholder.as_ref().map(|ls| ls.resolve_default()),
        "description": field.admin.description.as_ref().map(|ls| ls.resolve_default()),
        "readonly": field.admin.readonly || locale_locked,
        "localized": field.localized,
        "locale_locked": locale_locked,
    });

    if let Some(ref pos) = field.admin.position {
        ctx["position"] = json!(pos);
    }

    if let Some(err) = errors.get(&full_name) {
        ctx["error"] = json!(err);
    }

    apply_validation_props(field, &mut ctx);

    (ctx, full_name, value)
}

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
) -> Vec<Value> {
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

/// Build a field context for a single field definition, recursing into composite sub-fields.
///
/// `name_prefix`: the full form-name prefix for this field (e.g. `"content[0]"` for a
/// field inside a blocks row at index 0). Top-level fields use an empty prefix.
/// `depth`: current nesting depth (0 = top-level). Stops recursing at MAX_FIELD_DEPTH.
pub fn build_single_field_context(
    field: &FieldDefinition,
    values: &HashMap<String, String>,
    errors: &HashMap<String, String>,
    name_prefix: &str,
    non_default_locale: bool,
    depth: usize,
) -> Value {
    let (mut ctx, full_name, value) =
        build_base_field_context(field, values, errors, name_prefix, non_default_locale);

    // Beyond max depth, render as a simple text input
    if depth >= MAX_FIELD_DEPTH {
        return ctx;
    }

    let fc = SingleFieldCtx {
        field,
        value: &value,
        values,
        errors,
        name_prefix,
        full_name: &full_name,
        non_default_locale,
        depth,
    };

    apply_single_field_type(&mut ctx, &fc);

    ctx
}

/// Common params for single-field type handlers.
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

/// Dispatch type-specific context building to the appropriate handler.
fn apply_single_field_type(ctx: &mut Value, fc: &SingleFieldCtx) {
    match &fc.field.field_type {
        FieldType::Select | FieldType::Radio => single_select(ctx, fc),
        FieldType::Checkbox => single_checkbox(ctx, fc),
        FieldType::Relationship => single_relationship(ctx, fc),
        FieldType::Array => single_array(ctx, fc),
        FieldType::Group => single_group(ctx, fc),
        FieldType::Row => single_row(ctx, fc),
        FieldType::Collapsible => single_collapsible(ctx, fc),
        FieldType::Tabs => single_tabs(ctx, fc),
        FieldType::Date => single_date(ctx, fc),
        FieldType::Upload => single_upload(ctx, fc),
        FieldType::Richtext => single_richtext(ctx, fc),
        FieldType::Blocks => single_blocks(ctx, fc),
        FieldType::Join => single_join(ctx, fc),
        FieldType::Text | FieldType::Number if fc.field.has_many => single_tags(ctx, fc),
        _ => {}
    }
}

/// Build options list with selected state for Select/Radio fields.
fn single_select(ctx: &mut Value, fc: &SingleFieldCtx) {
    let (options, has_many) = build_select_options(fc.field, fc.value);
    ctx["options"] = json!(options);

    if has_many {
        ctx["has_many"] = json!(true);
    }
}

/// Set the checked state for Checkbox fields.
fn single_checkbox(ctx: &mut Value, fc: &SingleFieldCtx) {
    ctx["checked"] = json!(matches!(fc.value, "1" | "true" | "on" | "yes"));
}

/// Add collection reference, has_many, polymorphic, and picker for Relationship fields.
fn single_relationship(ctx: &mut Value, fc: &SingleFieldCtx) {
    if let Some(ref rc) = fc.field.relationship {
        ctx["relationship_collection"] = json!(rc.collection);
        ctx["has_many"] = json!(rc.has_many);

        if rc.is_polymorphic() {
            ctx["polymorphic"] = json!(true);
            ctx["collections"] = json!(rc.polymorphic);
        }
    }

    if let Some(ref p) = fc.field.admin.picker {
        ctx["picker"] = json!(p);
    }
}

/// Build template sub-fields, row props, and label_field for Array fields.
fn single_array(ctx: &mut Value, fc: &SingleFieldCtx) {
    let template_prefix = format!("{}[__INDEX__]", fc.full_name);
    let sub_fields: Vec<_> = fc
        .field
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

    ctx["sub_fields"] = json!(sub_fields);
    apply_row_props(fc.field, ctx, fc.full_name);

    if let Some(ref lf) = fc.field.admin.label_field {
        ctx["label_field"] = json!(lf);
    }
}

/// Recurse into Group sub-fields with `__` prefix naming and locale lock inheritance.
fn single_group(ctx: &mut Value, fc: &SingleFieldCtx) {
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

    let sub_fields: Vec<_> = fc
        .field
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
        .collect();

    ctx["sub_fields"] = json!(sub_fields);
    ctx["collapsed"] = json!(fc.field.admin.collapsed);
}

/// Build sub-fields for Row layout wrapper (transparent — no name added).
fn single_row(ctx: &mut Value, fc: &SingleFieldCtx) {
    ctx["sub_fields"] = json!(build_layout_sub_fields(
        &fc.field.fields,
        fc.values,
        fc.errors,
        fc.name_prefix,
        fc.full_name,
        fc.non_default_locale,
        fc.depth,
    ));
}

/// Build sub-fields for Collapsible layout wrapper with collapsed state.
fn single_collapsible(ctx: &mut Value, fc: &SingleFieldCtx) {
    ctx["sub_fields"] = json!(build_layout_sub_fields(
        &fc.field.fields,
        fc.values,
        fc.errors,
        fc.name_prefix,
        fc.full_name,
        fc.non_default_locale,
        fc.depth,
    ));
    ctx["collapsed"] = json!(fc.field.admin.collapsed);
}

/// Build tab panels with sub-fields and per-tab error counts.
fn single_tabs(ctx: &mut Value, fc: &SingleFieldCtx) {
    let tabs_ctx: Vec<_> = fc
        .field
        .tabs
        .iter()
        .map(|tab| {
            let tab_sub_fields = build_layout_sub_fields(
                &tab.fields,
                fc.values,
                fc.errors,
                fc.name_prefix,
                fc.full_name,
                fc.non_default_locale,
                fc.depth,
            );

            let error_count = count_errors_in_fields(&tab_sub_fields);
            let mut tab_ctx = json!({
                "label": &tab.label,
                "sub_fields": tab_sub_fields,
            });

            if error_count > 0 {
                tab_ctx["error_count"] = json!(error_count);
            }

            if let Some(ref desc) = tab.description {
                tab_ctx["description"] = json!(desc);
            }

            tab_ctx
        })
        .collect();

    ctx["tabs"] = json!(tabs_ctx);
}

/// Add picker appearance, UTC-to-local conversion, and timezone context for Date fields.
fn single_date(ctx: &mut Value, fc: &SingleFieldCtx) {
    let appearance = fc.field.picker_appearance.as_deref().unwrap_or("dayOnly");
    ctx["picker_appearance"] = json!(appearance);

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

    match appearance {
        "dayOnly" => {
            ctx["date_only_value"] = json!(display_value.get(..10).unwrap_or(&display_value))
        }
        "dayAndTime" => {
            ctx["datetime_local_value"] = json!(display_value.get(..16).unwrap_or(&display_value))
        }
        _ => {}
    }

    add_timezone_context(ctx, fc.field, tz_value, "");
}

/// Add collection reference, has_many, and picker for Upload fields.
fn single_upload(ctx: &mut Value, fc: &SingleFieldCtx) {
    if let Some(ref rc) = fc.field.relationship {
        ctx["relationship_collection"] = json!(rc.collection);

        if rc.has_many {
            ctx["has_many"] = json!(true);
        }
    }

    let picker = fc.field.admin.picker.as_deref().unwrap_or("drawer");

    if picker != "none" {
        ctx["picker"] = json!(picker);
    }
}

/// Add features, format, node names, and node attr errors for Richtext fields.
fn single_richtext(ctx: &mut Value, fc: &SingleFieldCtx) {
    ctx["resizable"] = json!(fc.field.admin.resizable);

    if !fc.field.admin.features.is_empty() {
        ctx["features"] = json!(fc.field.admin.features);
    }

    ctx["richtext_format"] = json!(fc.field.admin.richtext_format.as_deref().unwrap_or("html"));

    if !fc.field.admin.nodes.is_empty() {
        ctx["_node_names"] = json!(fc.field.admin.nodes);
    }

    if ctx.get("error").is_none_or(|v| v.is_null())
        && let Some(node_err) = collect_node_attr_errors(fc.errors, fc.full_name)
    {
        ctx["error"] = json!(node_err);
    }
}

/// Build block type definitions with template sub-fields, row props, and picker.
fn single_blocks(ctx: &mut Value, fc: &SingleFieldCtx) {
    let template_prefix = format!("{}[__INDEX__]", fc.full_name);
    let block_defs: Vec<_> = fc
        .field
        .blocks
        .iter()
        .map(|bd| {
            let block_fields: Vec<_> = bd
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

            let mut def = json!({
                "block_type": bd.block_type,
                "label": bd.label.as_ref().map(|ls| ls.resolve_default()).unwrap_or(&bd.block_type),
                "fields": block_fields,
            });

            if let Some(ref lf) = bd.label_field {
                def["label_field"] = json!(lf);
            }
            if let Some(ref g) = bd.group {
                def["group"] = json!(g);
            }
            if let Some(ref url) = bd.image_url {
                def["image_url"] = json!(url);
            }

            def
        })
        .collect();

    ctx["block_definitions"] = json!(block_defs);
    apply_row_props(fc.field, ctx, fc.full_name);

    if let Some(ref lf) = fc.field.admin.label_field {
        ctx["label_field"] = json!(lf);
    }

    if let Some(ref p) = fc.field.admin.picker {
        ctx["picker"] = json!(p);
    }
}

/// Add join collection/on reference for Join fields (always readonly).
fn single_join(ctx: &mut Value, fc: &SingleFieldCtx) {
    if let Some(ref jc) = fc.field.join {
        ctx["join_collection"] = json!(jc.collection);
        ctx["join_on"] = json!(jc.on);
    }

    ctx["readonly"] = json!(true);
}

/// Parse JSON array value into tag list for Text/Number has_many fields.
fn single_tags(ctx: &mut Value, fc: &SingleFieldCtx) {
    let tags: Vec<String> = from_str(fc.value).unwrap_or_default();

    ctx["has_many"] = json!(true);
    ctx["tags"] = json!(tags);
    ctx["value"] = json!(tags.join(","));
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

        let ctx = build_single_field_context(&field, &values, &errors, "", true, 0);

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

        let ctx = build_single_field_context(&field, &values, &errors, "", true, 0);

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
}
