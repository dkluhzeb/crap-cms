//! Apply type-specific extras to sub-field contexts (for composite field types).

use std::collections::HashMap;

use serde_json::{Value, from_str, json};

use crate::{
    admin::handlers::field_context::{
        MAX_FIELD_DEPTH, add_timezone_context,
        builder::{build_select_options, single::build_single_field_context},
        count_errors_in_fields, safe_template_id,
    },
    core::{FieldDefinition, FieldType},
    db::query::helpers::utc_to_local,
};

/// Parameters for recursive child-field building inside composite types
/// (Group, Array, Blocks, Tabs, etc.).
pub struct FieldRecursionCtx<'a> {
    pub values: &'a HashMap<String, String>,
    pub errors: &'a HashMap<String, String>,
    pub name_prefix: &'a str,
    pub non_default_locale: bool,
    pub depth: usize,
}

impl<'a> FieldRecursionCtx<'a> {
    pub fn builder(
        values: &'a HashMap<String, String>,
        errors: &'a HashMap<String, String>,
        name_prefix: &'a str,
    ) -> FieldRecursionCtxBuilder<'a> {
        FieldRecursionCtxBuilder {
            values,
            errors,
            name_prefix,
            non_default_locale: false,
            depth: 0,
        }
    }
}

/// Builder for [`FieldRecursionCtx`].
pub struct FieldRecursionCtxBuilder<'a> {
    values: &'a HashMap<String, String>,
    errors: &'a HashMap<String, String>,
    name_prefix: &'a str,
    non_default_locale: bool,
    depth: usize,
}

impl<'a> FieldRecursionCtxBuilder<'a> {
    pub fn non_default_locale(mut self, v: bool) -> Self {
        self.non_default_locale = v;
        self
    }

    pub fn depth(mut self, v: usize) -> Self {
        self.depth = v;
        self
    }

    pub fn build(self) -> FieldRecursionCtx<'a> {
        FieldRecursionCtx {
            values: self.values,
            errors: self.errors,
            name_prefix: self.name_prefix,
            non_default_locale: self.non_default_locale,
            depth: self.depth,
        }
    }
}

/// Recursively build sub-field contexts from a list of field definitions.
fn build_sub_fields(
    fields: &[FieldDefinition],
    values: &HashMap<String, String>,
    errors: &HashMap<String, String>,
    name_prefix: &str,
    non_default_locale: bool,
    depth: usize,
) -> Vec<Value> {
    fields
        .iter()
        .map(|nested| {
            build_single_field_context(
                nested,
                values,
                errors,
                name_prefix,
                non_default_locale,
                depth + 1,
            )
        })
        .collect()
}

/// Apply validation constraints (min/max length, min/max value, step, rows) to a field context.
pub(super) fn apply_validation_props(sf: &FieldDefinition, sub_ctx: &mut Value) {
    if let Some(ml) = sf.min_length {
        sub_ctx["min_length"] = json!(ml);
    }

    if let Some(ml) = sf.max_length {
        sub_ctx["max_length"] = json!(ml);
    }

    if let Some(v) = sf.min {
        sub_ctx["min"] = json!(v);
        sub_ctx["has_min"] = json!(true);
    }

    if let Some(v) = sf.max {
        sub_ctx["max"] = json!(v);
        sub_ctx["has_max"] = json!(true);
    }

    if sf.field_type == FieldType::Number {
        sub_ctx["step"] = json!(sf.admin.step.as_deref().unwrap_or("any"));
    }

    if sf.field_type == FieldType::Textarea {
        sub_ctx["rows"] = json!(sf.admin.rows.unwrap_or(8));
        sub_ctx["resizable"] = json!(sf.admin.resizable);
    }

    if sf.field_type == FieldType::Richtext {
        sub_ctx["resizable"] = json!(sf.admin.resizable);

        if !sf.admin.features.is_empty() {
            sub_ctx["features"] = json!(sf.admin.features);
        }

        sub_ctx["richtext_format"] = json!(sf.admin.richtext_format.as_deref().unwrap_or("html"));

        if !sf.admin.nodes.is_empty() {
            sub_ctx["_node_names"] = json!(sf.admin.nodes);
        }
    }

    if sf.field_type == FieldType::Date {
        if let Some(ref md) = sf.min_date {
            sub_ctx["min_date"] = json!(md);
        }

        if let Some(ref md) = sf.max_date {
            sub_ctx["max_date"] = json!(md);
        }
    }
}

/// Apply row-count constraints and labels shared by Array and Blocks fields.
pub(super) fn apply_row_props(sf: &FieldDefinition, sub_ctx: &mut Value, name_prefix: &str) {
    sub_ctx["row_count"] = json!(0);
    sub_ctx["template_id"] = json!(safe_template_id(name_prefix));

    if let Some(max) = sf.max_rows {
        sub_ctx["max_rows"] = json!(max);
    }

    if let Some(min) = sf.min_rows {
        sub_ctx["min_rows"] = json!(min);
    }

    sub_ctx["init_collapsed"] = json!(sf.admin.collapsed);

    if let Some(ref ls) = sf.admin.labels_singular {
        sub_ctx["add_label"] = json!(ls.resolve_default());
    }
}

/// Set checked state for Checkbox sub-fields.
fn apply_checkbox(value: &str, ctx: &mut Value) {
    ctx["checked"] = json!(matches!(value, "1" | "true" | "on" | "yes"));
}

/// Build options list with selected state for Select/Radio sub-fields.
fn apply_select(sf: &FieldDefinition, value: &str, ctx: &mut Value) {
    let (options, has_many) = build_select_options(sf, value);
    ctx["options"] = json!(options);

    if has_many {
        ctx["has_many"] = json!(true);
    }
}

/// Add picker appearance, UTC-to-local conversion, and timezone for Date sub-fields.
fn apply_date(sf: &FieldDefinition, value: &str, ctx: &mut Value, extras: &FieldRecursionCtx) {
    let appearance = sf.picker_appearance.as_deref().unwrap_or("dayOnly");
    ctx["picker_appearance"] = json!(appearance);

    let tz_key = format!("{}_tz", extras.name_prefix);
    let tz_value = extras.values.get(&tz_key).map(|s| s.as_str()).unwrap_or("");

    let display_value = if !tz_value.is_empty() && !value.is_empty() {
        utc_to_local(value, tz_value).unwrap_or_else(|| value.to_string())
    } else {
        value.to_string()
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

    add_timezone_context(ctx, sf, tz_value, "");
}

/// Build template sub-fields, row props, and label_field for Array sub-fields.
fn apply_array(sf: &FieldDefinition, ctx: &mut Value, extras: &FieldRecursionCtx) {
    let template_prefix = format!("{}[__INDEX__]", extras.name_prefix);
    ctx["sub_fields"] = json!(build_sub_fields(
        &sf.fields,
        &HashMap::new(),
        &HashMap::new(),
        &template_prefix,
        extras.non_default_locale,
        extras.depth,
    ));

    apply_row_props(sf, ctx, extras.name_prefix);

    if let Some(ref lf) = sf.admin.label_field {
        ctx["label_field"] = json!(lf);
    }
}

/// Recurse into Group sub-fields with current values/errors.
fn apply_group(sf: &FieldDefinition, ctx: &mut Value, extras: &FieldRecursionCtx) {
    ctx["sub_fields"] = json!(build_sub_fields(
        &sf.fields,
        extras.values,
        extras.errors,
        extras.name_prefix,
        extras.non_default_locale,
        extras.depth,
    ));
    ctx["collapsed"] = json!(sf.admin.collapsed);
}

/// Build sub-fields for Row layout wrapper.
fn apply_row(sf: &FieldDefinition, ctx: &mut Value, extras: &FieldRecursionCtx) {
    ctx["sub_fields"] = json!(build_sub_fields(
        &sf.fields,
        extras.values,
        extras.errors,
        extras.name_prefix,
        extras.non_default_locale,
        extras.depth,
    ));
}

/// Build sub-fields for Collapsible layout wrapper with collapsed state.
fn apply_collapsible(sf: &FieldDefinition, ctx: &mut Value, extras: &FieldRecursionCtx) {
    ctx["sub_fields"] = json!(build_sub_fields(
        &sf.fields,
        extras.values,
        extras.errors,
        extras.name_prefix,
        extras.non_default_locale,
        extras.depth,
    ));
    ctx["collapsed"] = json!(sf.admin.collapsed);
}

/// Build tab panels with sub-fields and per-tab error counts.
fn apply_tabs(sf: &FieldDefinition, ctx: &mut Value, extras: &FieldRecursionCtx) {
    let tabs_ctx: Vec<_> = sf
        .tabs
        .iter()
        .map(|tab| {
            let tab_sub_fields = build_sub_fields(
                &tab.fields,
                extras.values,
                extras.errors,
                extras.name_prefix,
                extras.non_default_locale,
                extras.depth,
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

/// Build block definitions with template sub-fields, row props, and picker.
fn apply_blocks(sf: &FieldDefinition, ctx: &mut Value, extras: &FieldRecursionCtx) {
    let template_prefix = format!("{}[__INDEX__]", extras.name_prefix);
    let block_defs: Vec<_> = sf
        .blocks
        .iter()
        .map(|bd| {
            let block_fields = build_sub_fields(
                &bd.fields,
                &HashMap::new(),
                &HashMap::new(),
                &template_prefix,
                extras.non_default_locale,
                extras.depth,
            );

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

    apply_row_props(sf, ctx, extras.name_prefix);

    if let Some(ref p) = sf.admin.picker {
        ctx["picker"] = json!(p);
    }
}

/// Add collection reference, has_many, polymorphic, and picker for Relationship sub-fields.
fn apply_relationship(sf: &FieldDefinition, ctx: &mut Value) {
    if let Some(ref rc) = sf.relationship {
        ctx["relationship_collection"] = json!(rc.collection);
        ctx["has_many"] = json!(rc.has_many);

        if rc.is_polymorphic() {
            ctx["polymorphic"] = json!(true);
            ctx["collections"] = json!(rc.polymorphic);
        }
    }

    if let Some(ref p) = sf.admin.picker {
        ctx["picker"] = json!(p);
    }
}

/// Add collection reference, has_many, and picker for Upload sub-fields.
fn apply_upload(sf: &FieldDefinition, ctx: &mut Value) {
    if let Some(ref rc) = sf.relationship {
        ctx["relationship_collection"] = json!(rc.collection);

        if rc.has_many {
            ctx["has_many"] = json!(true);
        }
    }

    let picker = sf.admin.picker.as_deref().unwrap_or("drawer");

    if picker != "none" {
        ctx["picker"] = json!(picker);
    }
}

/// Set the editor language for Code sub-fields plus the optional editor-time
/// language picker. Sources the chosen language in priority order:
///
///   1. Per-document value in the `<name_prefix><name>_lang` companion column.
///   2. Operator-configured default `admin.language`.
///   3. `"json"`.
///
/// When `admin.languages` is non-empty, the allow-list is also emitted as
/// `ctx["languages"]`. Mirrors `single_code` in `single.rs`.
fn apply_code(sf: &FieldDefinition, ctx: &mut Value, extras: &FieldRecursionCtx) {
    let default_lang = sf.admin.language.as_deref().unwrap_or("json");
    let full_name = if extras.name_prefix.is_empty() {
        sf.name.clone()
    } else {
        format!("{}.{}", extras.name_prefix, sf.name)
    };
    let chosen = extras
        .values
        .get(&format!("{}_lang", full_name))
        .map(String::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or(default_lang);
    ctx["language"] = json!(chosen);

    if !sf.admin.languages.is_empty() {
        ctx["languages"] = json!(sf.admin.languages);
    }
}

/// Parse JSON array value into tag list for Text/Number has_many sub-fields.
fn apply_tags(value: &str, ctx: &mut Value) {
    let tags: Vec<String> = from_str(value).unwrap_or_default();

    ctx["has_many"] = json!(true);
    ctx["tags"] = json!(tags);
    ctx["value"] = json!(tags.join(","));
}

/// Apply type-specific extras to an already-built sub_ctx (for top-level group sub-fields
/// that use the `col_name` pattern but still need composite-type recursion).
pub fn apply_field_type_extras(
    sf: &FieldDefinition,
    value: &str,
    sub_ctx: &mut Value,
    extras: &FieldRecursionCtx,
) {
    apply_validation_props(sf, sub_ctx);

    if extras.depth >= MAX_FIELD_DEPTH {
        return;
    }

    match &sf.field_type {
        FieldType::Checkbox => apply_checkbox(value, sub_ctx),
        FieldType::Select | FieldType::Radio => apply_select(sf, value, sub_ctx),
        FieldType::Date => apply_date(sf, value, sub_ctx, extras),
        FieldType::Array => apply_array(sf, sub_ctx, extras),
        FieldType::Group => apply_group(sf, sub_ctx, extras),
        FieldType::Row => apply_row(sf, sub_ctx, extras),
        FieldType::Collapsible => apply_collapsible(sf, sub_ctx, extras),
        FieldType::Tabs => apply_tabs(sf, sub_ctx, extras),
        FieldType::Blocks => apply_blocks(sf, sub_ctx, extras),
        FieldType::Relationship => apply_relationship(sf, sub_ctx),
        FieldType::Upload => apply_upload(sf, sub_ctx),
        FieldType::Code => apply_code(sf, sub_ctx, extras),
        FieldType::Text | FieldType::Number if sf.has_many => apply_tags(value, sub_ctx),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;

    use super::*;
    use crate::{
        admin::handlers::field_context::MAX_FIELD_DEPTH,
        core::field::{
            BlockDefinition, FieldDefinition, FieldType, LocalizedString, RelationshipConfig,
            SelectOption,
        },
    };

    fn make_field(name: &str, ft: FieldType) -> FieldDefinition {
        FieldDefinition::builder(name, ft).build()
    }

    #[test]
    fn checkbox_checked() {
        let sf = make_field("active", FieldType::Checkbox);
        let mut ctx = json!({"name": "group__active"});
        let (vals, errs) = (HashMap::new(), HashMap::new());
        let extras = FieldRecursionCtx::builder(&vals, &errs, "group__active").build();
        apply_field_type_extras(&sf, "true", &mut ctx, &extras);
        assert_eq!(ctx["checked"], true);
    }

    #[test]
    fn checkbox_unchecked() {
        let sf = make_field("active", FieldType::Checkbox);
        let mut ctx = json!({"name": "group__active"});
        let (vals, errs) = (HashMap::new(), HashMap::new());
        let extras = FieldRecursionCtx::builder(&vals, &errs, "group__active").build();
        apply_field_type_extras(&sf, "0", &mut ctx, &extras);
        assert_eq!(ctx["checked"], false);
    }

    #[test]
    fn select_marks_selected() {
        let mut sf = make_field("color", FieldType::Select);
        sf.options = vec![
            SelectOption::new(LocalizedString::Plain("Red".to_string()), "red"),
            SelectOption::new(LocalizedString::Plain("Green".to_string()), "green"),
        ];
        let mut ctx = json!({"name": "group__color"});
        let (vals, errs) = (HashMap::new(), HashMap::new());
        let extras = FieldRecursionCtx::builder(&vals, &errs, "group__color").build();
        apply_field_type_extras(&sf, "green", &mut ctx, &extras);
        let opts = ctx["options"].as_array().unwrap();
        assert_eq!(opts[0]["selected"], false);
        assert_eq!(opts[1]["selected"], true);
    }

    #[test]
    fn date_day_only() {
        let sf = make_field("d", FieldType::Date);
        let mut ctx = json!({"name": "group__d"});
        let (vals, errs) = (HashMap::new(), HashMap::new());
        let extras = FieldRecursionCtx::builder(&vals, &errs, "group__d").build();
        apply_field_type_extras(&sf, "2026-01-15T12:00:00Z", &mut ctx, &extras);
        assert_eq!(ctx["picker_appearance"], "dayOnly");
        assert_eq!(ctx["date_only_value"], "2026-01-15");
    }

    #[test]
    fn date_day_and_time() {
        let mut sf = make_field("d", FieldType::Date);
        sf.picker_appearance = Some("dayAndTime".to_string());
        let mut ctx = json!({"name": "group__d"});
        let (vals, errs) = (HashMap::new(), HashMap::new());
        let extras = FieldRecursionCtx::builder(&vals, &errs, "group__d").build();
        apply_field_type_extras(&sf, "2026-01-15T09:30:00Z", &mut ctx, &extras);
        assert_eq!(ctx["picker_appearance"], "dayAndTime");
        assert_eq!(ctx["datetime_local_value"], "2026-01-15T09:30");
    }

    #[test]
    fn date_short_values() {
        let sf = make_field("d", FieldType::Date);
        let mut ctx = json!({"name": "g__d"});
        let (vals, errs) = (HashMap::new(), HashMap::new());
        let extras = FieldRecursionCtx::builder(&vals, &errs, "g__d").build();
        apply_field_type_extras(&sf, "short", &mut ctx, &extras);
        assert_eq!(ctx["date_only_value"], "short");
    }

    #[test]
    fn relationship() {
        let mut sf = make_field("author", FieldType::Relationship);
        sf.relationship = Some(RelationshipConfig::new("users", true));
        let mut ctx = json!({"name": "group__author"});
        let (vals, errs) = (HashMap::new(), HashMap::new());
        let extras = FieldRecursionCtx::builder(&vals, &errs, "group__author").build();
        apply_field_type_extras(&sf, "", &mut ctx, &extras);
        assert_eq!(ctx["relationship_collection"], "users");
        assert_eq!(ctx["has_many"], true);
    }

    #[test]
    fn upload() {
        let mut sf = make_field("image", FieldType::Upload);
        sf.relationship = Some(RelationshipConfig::new("media", false));
        let mut ctx = json!({"name": "group__image"});
        let (vals, errs) = (HashMap::new(), HashMap::new());
        let extras = FieldRecursionCtx::builder(&vals, &errs, "group__image").build();
        apply_field_type_extras(&sf, "", &mut ctx, &extras);
        assert_eq!(ctx["relationship_collection"], "media");
        assert_eq!(ctx["picker"], "drawer");
    }

    #[test]
    fn array_in_group() {
        let mut arr = make_field("tags", FieldType::Array);
        arr.fields = vec![make_field("name", FieldType::Text)];
        arr.min_rows = Some(1);
        arr.max_rows = Some(3);
        arr.admin.collapsed = true;
        arr.admin.labels_singular = Some(LocalizedString::Plain("Tag".to_string()));
        arr.admin.label_field = Some("name".to_string());
        let mut ctx = json!({"name": "group__tags"});
        let (vals, errs) = (HashMap::new(), HashMap::new());
        let extras = FieldRecursionCtx::builder(&vals, &errs, "group__tags").build();
        apply_field_type_extras(&arr, "", &mut ctx, &extras);
        assert!(ctx["sub_fields"].as_array().is_some());
        assert_eq!(ctx["row_count"], 0);
        assert_eq!(ctx["min_rows"], 1);
        assert_eq!(ctx["max_rows"], 3);
        assert_eq!(ctx["init_collapsed"], true);
        assert_eq!(ctx["add_label"], "Tag");
        assert_eq!(ctx["label_field"], "name");
    }

    #[test]
    fn group_in_group() {
        let mut inner = make_field("meta", FieldType::Group);
        inner.fields = vec![make_field("author", FieldType::Text)];
        inner.admin.collapsed = true;
        let mut ctx = json!({"name": "outer__meta"});
        let (vals, errs) = (HashMap::new(), HashMap::new());
        let extras = FieldRecursionCtx::builder(&vals, &errs, "outer__meta").build();
        apply_field_type_extras(&inner, "", &mut ctx, &extras);
        assert!(ctx["sub_fields"].as_array().is_some());
        assert_eq!(ctx["collapsed"], true);
    }

    #[test]
    fn blocks_in_group() {
        let mut blk = make_field("sections", FieldType::Blocks);
        blk.blocks = vec![{
            let mut bd = BlockDefinition::new("text", vec![make_field("body", FieldType::Text)]);
            bd.label_field = Some("body".to_string());
            bd
        }];
        blk.min_rows = Some(0);
        blk.max_rows = Some(5);
        blk.admin.collapsed = true;
        blk.admin.labels_singular = Some(LocalizedString::Plain("Section".to_string()));
        let mut ctx = json!({"name": "group__sections"});
        let (vals, errs) = (HashMap::new(), HashMap::new());
        let extras = FieldRecursionCtx::builder(&vals, &errs, "group__sections").build();
        apply_field_type_extras(&blk, "", &mut ctx, &extras);
        assert!(ctx["block_definitions"].as_array().is_some());
        assert_eq!(ctx["row_count"], 0);
        assert_eq!(ctx["min_rows"], 0);
        assert_eq!(ctx["max_rows"], 5);
        assert_eq!(ctx["init_collapsed"], true);
        assert_eq!(ctx["add_label"], "Section");
        let bd = ctx["block_definitions"].as_array().unwrap();
        assert_eq!(bd[0]["label_field"], "body");
    }

    #[test]
    fn max_depth_stops_recursion() {
        let mut arr = make_field("deep", FieldType::Array);
        arr.fields = vec![make_field("leaf", FieldType::Text)];
        let mut ctx = json!({"name": "group__deep"});
        let (vals, errs) = (HashMap::new(), HashMap::new());
        let extras = FieldRecursionCtx::builder(&vals, &errs, "group__deep")
            .depth(MAX_FIELD_DEPTH)
            .build();
        apply_field_type_extras(&arr, "", &mut ctx, &extras);
        assert!(ctx.get("sub_fields").is_none());
    }

    #[test]
    fn unknown_type_is_noop() {
        let sf = make_field("body", FieldType::Richtext);
        let mut ctx = json!({"name": "group__body", "field_type": "richtext"});
        let (vals, errs) = (HashMap::new(), HashMap::new());
        let extras = FieldRecursionCtx::builder(&vals, &errs, "group__body").build();
        apply_field_type_extras(&sf, "hello", &mut ctx, &extras);
        assert!(ctx.get("options").is_none());
        assert!(ctx.get("checked").is_none());
    }
}
