//! Apply type-specific extras to sub-field contexts (for composite field types).

use std::collections::HashMap;

use serde_json::{Value, from_str, json};

use crate::core::{FieldDefinition, FieldType};

use super::super::{MAX_FIELD_DEPTH, count_errors_in_fields, safe_template_id};
use super::build_select_options;
use super::single::build_single_field_context;

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

/// Apply type-specific extras to an already-built sub_ctx (for top-level group sub-fields
/// that use the `col_name` pattern but still need composite-type recursion).
pub fn apply_field_type_extras(
    sf: &FieldDefinition,
    value: &str,
    sub_ctx: &mut Value,
    extras: &FieldRecursionCtx,
) {
    let values = extras.values;
    let errors = extras.errors;
    let name_prefix = extras.name_prefix;
    let non_default_locale = extras.non_default_locale;
    let depth = extras.depth;

    // Validation property context for sub-fields
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
        let step = sf.admin.step.as_deref().unwrap_or("any");
        sub_ctx["step"] = json!(step);
    }

    if sf.field_type == FieldType::Textarea {
        let rows = sf.admin.rows.unwrap_or(8);
        sub_ctx["rows"] = json!(rows);
        sub_ctx["resizable"] = json!(sf.admin.resizable);
    }

    if sf.field_type == FieldType::Richtext {
        sub_ctx["resizable"] = json!(sf.admin.resizable);

        if !sf.admin.features.is_empty() {
            sub_ctx["features"] = json!(sf.admin.features);
        }

        let fmt = sf.admin.richtext_format.as_deref().unwrap_or("html");

        sub_ctx["richtext_format"] = json!(fmt);

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

    if depth >= MAX_FIELD_DEPTH {
        return;
    }

    match &sf.field_type {
        FieldType::Checkbox => {
            let checked = matches!(value, "1" | "true" | "on" | "yes");
            sub_ctx["checked"] = json!(checked);
        }
        FieldType::Select | FieldType::Radio => {
            let (options, has_many) = build_select_options(sf, value);
            sub_ctx["options"] = json!(options);
            if has_many {
                sub_ctx["has_many"] = json!(true);
            }
        }
        FieldType::Date => {
            let appearance = sf.picker_appearance.as_deref().unwrap_or("dayOnly");

            sub_ctx["picker_appearance"] = json!(appearance);

            let tz_key = format!("{}_tz", name_prefix);
            let tz_value = values.get(&tz_key).map(|s| s.as_str()).unwrap_or("");

            // Convert UTC back to local time for display if timezone is stored
            let display_value = if !tz_value.is_empty() && !value.is_empty() {
                crate::db::query::helpers::utc_to_local(value, tz_value)
                    .unwrap_or_else(|| value.to_string())
            } else {
                value.to_string()
            };

            match appearance {
                "dayOnly" => {
                    let date_val = display_value.get(..10).unwrap_or(&display_value);
                    sub_ctx["date_only_value"] = json!(date_val);
                }
                "dayAndTime" => {
                    let dt_val = display_value.get(..16).unwrap_or(&display_value);
                    sub_ctx["datetime_local_value"] = json!(dt_val);
                }
                _ => {}
            }

            super::super::add_timezone_context(sub_ctx, sf, tz_value, "");
        }
        FieldType::Array => {
            let template_prefix = format!("{}[__INDEX__]", name_prefix);
            let sub_fields: Vec<_> = sf
                .fields
                .iter()
                .map(|nested| {
                    build_single_field_context(
                        nested,
                        &HashMap::new(),
                        &HashMap::new(),
                        &template_prefix,
                        non_default_locale,
                        depth + 1,
                    )
                })
                .collect();

            sub_ctx["sub_fields"] = json!(sub_fields);
            sub_ctx["row_count"] = json!(0);
            sub_ctx["template_id"] = json!(safe_template_id(name_prefix));

            if let Some(ref lf) = sf.admin.label_field {
                sub_ctx["label_field"] = json!(lf);
            }

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
        FieldType::Group => {
            let sub_fields: Vec<_> = sf
                .fields
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
                .collect();

            sub_ctx["sub_fields"] = json!(sub_fields);
            sub_ctx["collapsed"] = json!(sf.admin.collapsed);
        }
        FieldType::Row => {
            let sub_fields: Vec<_> = sf
                .fields
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
                .collect();

            sub_ctx["sub_fields"] = json!(sub_fields);
        }
        FieldType::Collapsible => {
            let sub_fields: Vec<_> = sf
                .fields
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
                .collect();

            sub_ctx["sub_fields"] = json!(sub_fields);
            sub_ctx["collapsed"] = json!(sf.admin.collapsed);
        }
        FieldType::Tabs => {
            let tabs_ctx: Vec<_> = sf
                .tabs
                .iter()
                .map(|tab| {
                    let tab_sub_fields: Vec<_> = tab
                        .fields
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
                        .collect();

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

            sub_ctx["tabs"] = json!(tabs_ctx);
        }
        FieldType::Blocks => {
            let block_defs: Vec<_> = sf
                .blocks
                .iter()
                .map(|bd| {
                    let template_prefix = format!("{}[__INDEX__]", name_prefix);
                    let block_fields: Vec<_> = bd
                        .fields
                        .iter()
                        .map(|nested| {
                            build_single_field_context(
                                nested,
                                &HashMap::new(),
                                &HashMap::new(),
                                &template_prefix,
                                non_default_locale,
                                depth + 1,
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

            sub_ctx["block_definitions"] = json!(block_defs);
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

            if let Some(ref p) = sf.admin.picker {
                sub_ctx["picker"] = json!(p);
            }
        }
        FieldType::Relationship => {
            if let Some(ref rc) = sf.relationship {
                sub_ctx["relationship_collection"] = json!(rc.collection);
                sub_ctx["has_many"] = json!(rc.has_many);

                if rc.is_polymorphic() {
                    sub_ctx["polymorphic"] = json!(true);
                    sub_ctx["collections"] = json!(rc.polymorphic);
                }
            }

            if let Some(ref p) = sf.admin.picker {
                sub_ctx["picker"] = json!(p);
            }
        }
        FieldType::Upload => {
            if let Some(ref rc) = sf.relationship {
                sub_ctx["relationship_collection"] = json!(rc.collection);

                if rc.has_many {
                    sub_ctx["has_many"] = json!(true);
                }
            }

            let picker = sf.admin.picker.as_deref().unwrap_or("drawer");
            if picker != "none" {
                sub_ctx["picker"] = json!(picker);
            }
        }
        FieldType::Code => {
            let lang = sf.admin.language.as_deref().unwrap_or("json");
            sub_ctx["language"] = json!(lang);
        }
        FieldType::Text | FieldType::Number if sf.has_many => {
            let tags: Vec<String> = from_str(value).unwrap_or_default();
            sub_ctx["has_many"] = json!(true);
            sub_ctx["tags"] = json!(tags);
            sub_ctx["value"] = json!(tags.join(","));
        }
        _ => {}
    }
}
