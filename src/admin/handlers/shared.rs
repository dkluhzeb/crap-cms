//! Shared helper functions for admin handlers (collections + globals).

use axum::{
    http::StatusCode,
    response::{Html, IntoResponse, Redirect},
    Extension,
};
use serde::Deserialize;
use std::collections::HashMap;

use crate::admin::AdminState;
use crate::admin::context::{ContextBuilder, PageType};
use crate::core::auth::AuthUser;
use crate::core::field::FieldType;
use crate::core::upload;
use crate::db::query::{self, AccessResult, LocaleContext};

/// Query parameters for paginated collection list views.
#[derive(Debug, Deserialize)]
pub struct PaginationParams {
    pub page: Option<i64>,
    pub per_page: Option<i64>,
    pub search: Option<String>,
}

/// Query parameters for locale selection on edit pages.
#[derive(Debug, Deserialize)]
pub struct LocaleParams {
    pub locale: Option<String>,
}

/// Extract the user document from AuthUser extension (for access checks).
pub(super) fn get_user_doc(auth_user: &Option<Extension<AuthUser>>) -> Option<&crate::core::Document> {
    auth_user.as_ref().map(|Extension(au)| &au.user_doc)
}

/// Strip denied fields from a document's fields map.
pub(super) fn strip_denied_fields(
    fields: &mut HashMap<String, serde_json::Value>,
    denied: &[String],
) {
    for name in denied {
        fields.remove(name);
    }
}

/// Helper to check collection/global-level access. Returns AccessResult or renders a 403 page.
#[allow(clippy::result_large_err)]
pub(super) fn check_access_or_forbid(
    state: &AdminState,
    access_ref: Option<&str>,
    auth_user: &Option<Extension<AuthUser>>,
    id: Option<&str>,
    data: Option<&HashMap<String, serde_json::Value>>,
) -> Result<AccessResult, axum::response::Response> {
    let user_doc = get_user_doc(auth_user);
    let conn = state.pool.get()
        .map_err(|_| forbidden(state, "Database error").into_response())?;
    state.hook_runner.check_access(access_ref, user_doc, id, data, &conn)
        .map_err(|e| {
            tracing::error!("Access check error: {}", e);
            forbidden(state, "Access check failed").into_response()
        })
}

/// Build locale template context (selector data) from config + current locale.
/// Returns `(locale_ctx_for_db, template_json)` where template_json has
/// `has_locales`, `current_locale`, `locales` (array with value/label/selected).
pub(super) fn build_locale_template_data(
    state: &AdminState,
    requested_locale: Option<&str>,
) -> (Option<LocaleContext>, serde_json::Value) {
    let config = &state.config.locale;
    if !config.is_enabled() {
        return (None, serde_json::json!({}));
    }
    let current = requested_locale.unwrap_or(&config.default_locale);
    let locale_ctx = LocaleContext::from_locale_string(Some(current), config);
    let locales: Vec<serde_json::Value> = config.locales.iter().map(|l| {
        serde_json::json!({
            "value": l,
            "label": l.to_uppercase(),
            "selected": l == current,
        })
    }).collect();
    let data = serde_json::json!({
        "has_locales": true,
        "current_locale": current,
        "locales": locales,
    });
    (locale_ctx, data)
}

/// Auto-generate a label from a field name (e.g. "my_field" → "My Field").
pub(super) fn auto_label_from_name(name: &str) -> String {
    name.split('_')
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                None => String::new(),
                Some(f) => f.to_uppercase().chain(c).collect(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Check if the current locale is a non-default locale (fields should be locked).
pub(super) fn is_non_default_locale(state: &AdminState, requested_locale: Option<&str>) -> bool {
    let config = &state.config.locale;
    if !config.is_enabled() {
        return false;
    }
    let current = requested_locale.unwrap_or(&config.default_locale);
    current != config.default_locale
}

/// Make a template-ID-safe string from a field name (replaces `[`, `]` with `-`).
fn safe_template_id(name: &str) -> String {
    name.replace('[', "-").replace(']', "")
}

/// Max nesting depth for recursive field context building (guard against infinite nesting).
const MAX_FIELD_DEPTH: usize = 5;

/// Build a field context for a single field definition, recursing into composite sub-fields.
///
/// `name_prefix`: the full form-name prefix for this field (e.g. `"content[0]"` for a
/// field inside a blocks row at index 0). Top-level fields use an empty prefix.
/// `depth`: current nesting depth (0 = top-level). Stops recursing at MAX_FIELD_DEPTH.
fn build_single_field_context(
    field: &crate::core::field::FieldDefinition,
    values: &HashMap<String, String>,
    errors: &HashMap<String, String>,
    name_prefix: &str,
    non_default_locale: bool,
    depth: usize,
) -> serde_json::Value {
    let full_name = if name_prefix.is_empty() {
        field.name.clone()
    } else {
        format!("{}[{}]", name_prefix, field.name)
    };
    let value = values.get(&full_name).cloned().unwrap_or_default();
    let label = field.admin.label.as_ref()
        .map(|ls| ls.resolve_default().to_string())
        .unwrap_or_else(|| auto_label_from_name(&field.name));
    let locale_locked = non_default_locale && !field.localized;

    let mut ctx = serde_json::json!({
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

    if let Some(err) = errors.get(&full_name) {
        ctx["error"] = serde_json::json!(err);
    }

    // Beyond max depth, render as a simple text input
    if depth >= MAX_FIELD_DEPTH {
        return ctx;
    }

    match &field.field_type {
        FieldType::Select => {
            let options: Vec<_> = field.options.iter().map(|opt| {
                serde_json::json!({
                    "label": opt.label.resolve_default(),
                    "value": opt.value,
                    "selected": opt.value == value,
                })
            }).collect();
            ctx["options"] = serde_json::json!(options);
        }
        FieldType::Checkbox => {
            let checked = matches!(value.as_str(), "1" | "true" | "on" | "yes");
            ctx["checked"] = serde_json::json!(checked);
        }
        FieldType::Relationship => {
            if let Some(ref rc) = field.relationship {
                ctx["relationship_collection"] = serde_json::json!(rc.collection);
                ctx["has_many"] = serde_json::json!(rc.has_many);
            }
        }
        FieldType::Array => {
            // Build sub_field contexts for the <template> section (with __INDEX__ placeholder)
            let template_prefix = format!("{}[__INDEX__]", full_name);
            let sub_fields: Vec<_> = field.fields.iter().map(|sf| {
                build_single_field_context(sf, &HashMap::new(), &HashMap::new(), &template_prefix, non_default_locale, depth + 1)
            }).collect();
            ctx["sub_fields"] = serde_json::json!(sub_fields);
            ctx["row_count"] = serde_json::json!(0);
            ctx["template_id"] = serde_json::json!(safe_template_id(&full_name));
        }
        FieldType::Group => {
            // Group sub-fields use double-underscore naming at top level,
            // but when nested inside Array/Blocks they use bracketed names.
            let sub_fields: Vec<_> = if name_prefix.is_empty() {
                // Top-level group: use col_name pattern (group__subfield)
                field.fields.iter().map(|sf| {
                    let col_name = format!("{}__{}", field.name, sf.name);
                    let sub_value = values.get(&col_name).cloned().unwrap_or_default();
                    let sub_label = sf.admin.label.as_ref()
                        .map(|ls| ls.resolve_default().to_string())
                        .unwrap_or_else(|| auto_label_from_name(&sf.name));
                    let sf_locale_locked = non_default_locale && !field.localized;
                    let mut sub_ctx = serde_json::json!({
                        "name": col_name,
                        "field_type": sf.field_type.as_str(),
                        "label": sub_label,
                        "required": sf.required,
                        "value": sub_value,
                        "placeholder": sf.admin.placeholder.as_ref().map(|ls| ls.resolve_default()),
                        "description": sf.admin.description.as_ref().map(|ls| ls.resolve_default()),
                        "readonly": sf.admin.readonly || sf_locale_locked,
                        "localized": field.localized,
                        "locale_locked": sf_locale_locked,
                    });
                    // Recurse for nested composites
                    apply_field_type_extras(sf, &sub_value, &mut sub_ctx, values, errors, &col_name, non_default_locale, depth + 1);
                    sub_ctx
                }).collect()
            } else {
                // Nested group: use bracketed naming via recursion
                field.fields.iter().map(|sf| {
                    build_single_field_context(sf, values, errors, &full_name, non_default_locale, depth + 1)
                }).collect()
            };
            ctx["sub_fields"] = serde_json::json!(sub_fields);
            if field.admin.collapsed {
                ctx["collapsed"] = serde_json::json!(true);
            }
        }
        FieldType::Date => {
            let appearance = field.picker_appearance.as_deref().unwrap_or("dayOnly");
            ctx["picker_appearance"] = serde_json::json!(appearance);
            match appearance {
                "dayOnly" => {
                    let date_val = if value.len() >= 10 { &value[..10] } else { &value };
                    ctx["date_only_value"] = serde_json::json!(date_val);
                }
                "dayAndTime" => {
                    let dt_val = if value.len() >= 16 { &value[..16] } else { &value };
                    ctx["datetime_local_value"] = serde_json::json!(dt_val);
                }
                _ => {}
            }
        }
        FieldType::Upload => {
            if let Some(ref rc) = field.relationship {
                ctx["relationship_collection"] = serde_json::json!(rc.collection);
            }
        }
        FieldType::Blocks => {
            let block_defs: Vec<_> = field.blocks.iter().map(|bd| {
                // Build sub-field contexts for each block type's <template> section
                let template_prefix = format!("{}[__INDEX__]", full_name);
                let block_fields: Vec<_> = bd.fields.iter().map(|sf| {
                    build_single_field_context(sf, &HashMap::new(), &HashMap::new(), &template_prefix, non_default_locale, depth + 1)
                }).collect();
                serde_json::json!({
                    "block_type": bd.block_type,
                    "label": bd.label.as_ref().map(|ls| ls.resolve_default()).unwrap_or(&bd.block_type),
                    "fields": block_fields,
                })
            }).collect();
            ctx["block_definitions"] = serde_json::json!(block_defs);
            ctx["row_count"] = serde_json::json!(0);
            ctx["template_id"] = serde_json::json!(safe_template_id(&full_name));
        }
        _ => {}
    }

    ctx
}

/// Apply type-specific extras to an already-built sub_ctx (for top-level group sub-fields
/// that use the `col_name` pattern but still need composite-type recursion).
fn apply_field_type_extras(
    sf: &crate::core::field::FieldDefinition,
    value: &str,
    sub_ctx: &mut serde_json::Value,
    values: &HashMap<String, String>,
    errors: &HashMap<String, String>,
    name_prefix: &str,
    non_default_locale: bool,
    depth: usize,
) {
    if depth >= MAX_FIELD_DEPTH { return; }
    match &sf.field_type {
        FieldType::Checkbox => {
            let checked = matches!(value, "1" | "true" | "on" | "yes");
            sub_ctx["checked"] = serde_json::json!(checked);
        }
        FieldType::Select => {
            let options: Vec<_> = sf.options.iter().map(|opt| {
                serde_json::json!({
                    "label": opt.label.resolve_default(),
                    "value": opt.value,
                    "selected": opt.value == value,
                })
            }).collect();
            sub_ctx["options"] = serde_json::json!(options);
        }
        FieldType::Date => {
            let appearance = sf.picker_appearance.as_deref().unwrap_or("dayOnly");
            sub_ctx["picker_appearance"] = serde_json::json!(appearance);
            match appearance {
                "dayOnly" => {
                    let date_val = if value.len() >= 10 { &value[..10] } else { value };
                    sub_ctx["date_only_value"] = serde_json::json!(date_val);
                }
                "dayAndTime" => {
                    let dt_val = if value.len() >= 16 { &value[..16] } else { value };
                    sub_ctx["datetime_local_value"] = serde_json::json!(dt_val);
                }
                _ => {}
            }
        }
        FieldType::Array => {
            let template_prefix = format!("{}[__INDEX__]", name_prefix);
            let sub_fields: Vec<_> = sf.fields.iter().map(|nested| {
                build_single_field_context(nested, &HashMap::new(), &HashMap::new(), &template_prefix, non_default_locale, depth + 1)
            }).collect();
            sub_ctx["sub_fields"] = serde_json::json!(sub_fields);
            sub_ctx["row_count"] = serde_json::json!(0);
            sub_ctx["template_id"] = serde_json::json!(safe_template_id(name_prefix));
        }
        FieldType::Group => {
            let sub_fields: Vec<_> = sf.fields.iter().map(|nested| {
                build_single_field_context(nested, values, errors, name_prefix, non_default_locale, depth + 1)
            }).collect();
            sub_ctx["sub_fields"] = serde_json::json!(sub_fields);
            if sf.admin.collapsed {
                sub_ctx["collapsed"] = serde_json::json!(true);
            }
        }
        FieldType::Blocks => {
            let block_defs: Vec<_> = sf.blocks.iter().map(|bd| {
                let template_prefix = format!("{}[__INDEX__]", name_prefix);
                let block_fields: Vec<_> = bd.fields.iter().map(|nested| {
                    build_single_field_context(nested, &HashMap::new(), &HashMap::new(), &template_prefix, non_default_locale, depth + 1)
                }).collect();
                serde_json::json!({
                    "block_type": bd.block_type,
                    "label": bd.label.as_ref().map(|ls| ls.resolve_default()).unwrap_or(&bd.block_type),
                    "fields": block_fields,
                })
            }).collect();
            sub_ctx["block_definitions"] = serde_json::json!(block_defs);
            sub_ctx["row_count"] = serde_json::json!(0);
            sub_ctx["template_id"] = serde_json::json!(safe_template_id(name_prefix));
        }
        FieldType::Relationship => {
            if let Some(ref rc) = sf.relationship {
                sub_ctx["relationship_collection"] = serde_json::json!(rc.collection);
                sub_ctx["has_many"] = serde_json::json!(rc.has_many);
            }
        }
        FieldType::Upload => {
            if let Some(ref rc) = sf.relationship {
                sub_ctx["relationship_collection"] = serde_json::json!(rc.collection);
            }
        }
        _ => {}
    }
}

/// Build field context objects for template rendering.
///
/// `non_default_locale`: when true, non-localized fields are rendered readonly
/// (locked) because they are shared across all locales and should only be edited
/// from the default locale.
pub(super) fn build_field_contexts(
    fields: &[crate::core::field::FieldDefinition],
    values: &HashMap<String, String>,
    errors: &HashMap<String, String>,
    filter_hidden: bool,
    non_default_locale: bool,
) -> Vec<serde_json::Value> {
    let iter: Box<dyn Iterator<Item = &crate::core::field::FieldDefinition>> = if filter_hidden {
        Box::new(fields.iter().filter(|field| !field.admin.hidden))
    } else {
        Box::new(fields.iter())
    };
    iter.map(|field| {
        build_single_field_context(field, values, errors, "", non_default_locale, 0)
    }).collect()
}

/// Build a sub-field context for a single field within an array/blocks row,
/// recursively handling nested composite sub-fields.
///
/// `sf`: the sub-field definition
/// `raw_value`: the raw JSON value for this sub-field from the hydrated document
/// `parent_name`: the parent field's name (e.g. "content")
/// `idx`: the row index within the parent
/// `locale_locked`: whether the parent is locale-locked
/// `non_default_locale`: whether we're on a non-default locale
/// `depth`: nesting depth
fn build_enriched_sub_field_context(
    sf: &crate::core::field::FieldDefinition,
    raw_value: Option<&serde_json::Value>,
    parent_name: &str,
    idx: usize,
    locale_locked: bool,
    non_default_locale: bool,
    depth: usize,
) -> serde_json::Value {
    let indexed_name = format!("{}[{}][{}]", parent_name, idx, sf.name);

    // For scalar types, stringify the value. For composites, keep structured.
    let val = raw_value
        .map(|v| match v {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Null => String::new(),
            other => {
                match sf.field_type {
                    FieldType::Array | FieldType::Blocks | FieldType::Group => String::new(),
                    _ => other.to_string(),
                }
            }
        })
        .unwrap_or_default();

    let sf_label = sf.admin.label.as_ref()
        .map(|ls| ls.resolve_default().to_string())
        .unwrap_or_else(|| auto_label_from_name(&sf.name));

    let mut sub_ctx = serde_json::json!({
        "name": indexed_name,
        "field_type": sf.field_type.as_str(),
        "label": sf_label,
        "value": val,
        "required": sf.required,
        "readonly": sf.admin.readonly || locale_locked,
        "locale_locked": locale_locked,
        "placeholder": sf.admin.placeholder.as_ref().map(|ls| ls.resolve_default()),
        "description": sf.admin.description.as_ref().map(|ls| ls.resolve_default()),
    });

    if depth >= MAX_FIELD_DEPTH { return sub_ctx; }

    match &sf.field_type {
        FieldType::Checkbox => {
            let checked = matches!(val.as_str(), "1" | "true" | "on" | "yes");
            sub_ctx["checked"] = serde_json::json!(checked);
        }
        FieldType::Select => {
            let options: Vec<_> = sf.options.iter().map(|opt| {
                serde_json::json!({
                    "label": opt.label.resolve_default(),
                    "value": opt.value,
                    "selected": opt.value == val,
                })
            }).collect();
            sub_ctx["options"] = serde_json::json!(options);
        }
        FieldType::Date => {
            let appearance = sf.picker_appearance.as_deref().unwrap_or("dayOnly");
            sub_ctx["picker_appearance"] = serde_json::json!(appearance);
            match appearance {
                "dayOnly" => {
                    let date_val = if val.len() >= 10 { &val[..10] } else { &val };
                    sub_ctx["date_only_value"] = serde_json::json!(date_val);
                }
                "dayAndTime" => {
                    let dt_val = if val.len() >= 16 { &val[..16] } else { &val };
                    sub_ctx["datetime_local_value"] = serde_json::json!(dt_val);
                }
                _ => {}
            }
        }
        FieldType::Relationship => {
            if let Some(ref rc) = sf.relationship {
                sub_ctx["relationship_collection"] = serde_json::json!(rc.collection);
                sub_ctx["has_many"] = serde_json::json!(rc.has_many);
            }
        }
        FieldType::Upload => {
            if let Some(ref rc) = sf.relationship {
                sub_ctx["relationship_collection"] = serde_json::json!(rc.collection);
            }
        }
        FieldType::Array => {
            // Nested array: recurse into sub-rows
            let nested_rows: Vec<serde_json::Value> = match raw_value {
                Some(serde_json::Value::Array(arr)) => {
                    arr.iter().enumerate().map(|(nested_idx, nested_row)| {
                        let nested_row_obj = nested_row.as_object();
                        let nested_sub_values: Vec<_> = sf.fields.iter().map(|nested_sf| {
                            let nested_raw = nested_row_obj.and_then(|m| m.get(&nested_sf.name));
                            build_enriched_sub_field_context(
                                nested_sf, nested_raw, &indexed_name, nested_idx,
                                locale_locked, non_default_locale, depth + 1,
                            )
                        }).collect();
                        serde_json::json!({
                            "index": nested_idx,
                            "sub_fields": nested_sub_values,
                        })
                    }).collect()
                }
                _ => Vec::new(),
            };
            // Template sub_fields for the nested <template> section
            let template_prefix = format!("{}[__INDEX__]", indexed_name);
            let template_sub_fields: Vec<_> = sf.fields.iter().map(|nested_sf| {
                build_single_field_context(nested_sf, &HashMap::new(), &HashMap::new(), &template_prefix, non_default_locale, depth + 1)
            }).collect();
            sub_ctx["sub_fields"] = serde_json::json!(template_sub_fields);
            sub_ctx["rows"] = serde_json::json!(nested_rows);
            sub_ctx["row_count"] = serde_json::json!(nested_rows.len());
            sub_ctx["template_id"] = serde_json::json!(safe_template_id(&indexed_name));
        }
        FieldType::Blocks => {
            // Nested blocks: recurse into block rows
            let nested_rows: Vec<serde_json::Value> = match raw_value {
                Some(serde_json::Value::Array(arr)) => {
                    arr.iter().enumerate().map(|(nested_idx, nested_row)| {
                        let nested_row_obj = nested_row.as_object();
                        let block_type = nested_row_obj
                            .and_then(|m| m.get("_block_type"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown");
                        let block_label = sf.blocks.iter()
                            .find(|bd| bd.block_type == block_type)
                            .and_then(|bd| bd.label.as_ref().map(|ls| ls.resolve_default()))
                            .unwrap_or(block_type);
                        let block_def = sf.blocks.iter().find(|bd| bd.block_type == block_type);
                        let nested_sub_values: Vec<_> = block_def
                            .map(|bd| bd.fields.iter().map(|nested_sf| {
                                let nested_raw = nested_row_obj.and_then(|m| m.get(&nested_sf.name));
                                build_enriched_sub_field_context(
                                    nested_sf, nested_raw, &indexed_name, nested_idx,
                                    locale_locked, non_default_locale, depth + 1,
                                )
                            }).collect())
                            .unwrap_or_default();
                        serde_json::json!({
                            "index": nested_idx,
                            "_block_type": block_type,
                            "block_label": block_label,
                            "sub_fields": nested_sub_values,
                        })
                    }).collect()
                }
                _ => Vec::new(),
            };
            // Block definitions for the nested <template> sections
            let block_defs: Vec<_> = sf.blocks.iter().map(|bd| {
                let template_prefix = format!("{}[__INDEX__]", indexed_name);
                let block_fields: Vec<_> = bd.fields.iter().map(|nested_sf| {
                    build_single_field_context(nested_sf, &HashMap::new(), &HashMap::new(), &template_prefix, non_default_locale, depth + 1)
                }).collect();
                serde_json::json!({
                    "block_type": bd.block_type,
                    "label": bd.label.as_ref().map(|ls| ls.resolve_default()).unwrap_or(&bd.block_type),
                    "fields": block_fields,
                })
            }).collect();
            sub_ctx["block_definitions"] = serde_json::json!(block_defs);
            sub_ctx["rows"] = serde_json::json!(nested_rows);
            sub_ctx["row_count"] = serde_json::json!(nested_rows.len());
            sub_ctx["template_id"] = serde_json::json!(safe_template_id(&indexed_name));
        }
        FieldType::Group => {
            // Nested group: sub-fields are stored as keys in the same row object
            let group_obj = match raw_value {
                Some(serde_json::Value::Object(_)) => raw_value,
                _ => None,
            };
            let nested_sub_fields: Vec<_> = sf.fields.iter().map(|nested_sf| {
                let nested_raw = group_obj
                    .and_then(|v| v.as_object())
                    .and_then(|m| m.get(&nested_sf.name));
                let nested_name = format!("{}[{}]", indexed_name, nested_sf.name);
                let nested_val = nested_raw
                    .map(|v| match v {
                        serde_json::Value::String(s) => s.clone(),
                        serde_json::Value::Null => String::new(),
                        other => other.to_string(),
                    })
                    .unwrap_or_default();
                let nested_label = nested_sf.admin.label.as_ref()
                    .map(|ls| ls.resolve_default().to_string())
                    .unwrap_or_else(|| auto_label_from_name(&nested_sf.name));
                let mut nested_ctx = serde_json::json!({
                    "name": nested_name,
                    "field_type": nested_sf.field_type.as_str(),
                    "label": nested_label,
                    "value": nested_val,
                    "required": nested_sf.required,
                    "readonly": nested_sf.admin.readonly || locale_locked,
                    "locale_locked": locale_locked,
                    "placeholder": nested_sf.admin.placeholder.as_ref().map(|ls| ls.resolve_default()),
                    "description": nested_sf.admin.description.as_ref().map(|ls| ls.resolve_default()),
                });
                apply_field_type_extras(
                    nested_sf, &nested_val, &mut nested_ctx,
                    &HashMap::new(), &HashMap::new(), &nested_name,
                    non_default_locale, depth + 1,
                );
                nested_ctx
            }).collect();
            sub_ctx["sub_fields"] = serde_json::json!(nested_sub_fields);
            if sf.admin.collapsed {
                sub_ctx["collapsed"] = serde_json::json!(true);
            }
        }
        _ => {}
    }

    sub_ctx
}

/// Enrich field contexts with data that requires DB access:
/// - Relationship fields: fetch available options from related collection
/// - Array fields: populate existing rows from hydrated document data
/// - Upload fields: fetch upload collection options with thumbnails
/// - Blocks fields: populate block rows from hydrated document data
pub(super) fn enrich_field_contexts(
    fields: &mut [serde_json::Value],
    field_defs: &[crate::core::field::FieldDefinition],
    doc_fields: &HashMap<String, serde_json::Value>,
    state: &AdminState,
    filter_hidden: bool,
    non_default_locale: bool,
) {
    let reg = match state.registry.read() {
        Ok(r) => r,
        Err(_) => return,
    };
    let conn = match state.pool.get() {
        Ok(c) => c,
        Err(_) => return,
    };

    let rel_locale_ctx = LocaleContext::from_locale_string(None, &state.config.locale);

    let defs_iter: Box<dyn Iterator<Item = &crate::core::field::FieldDefinition>> = if filter_hidden {
        Box::new(field_defs.iter().filter(|f| !f.admin.hidden))
    } else {
        Box::new(field_defs.iter())
    };

    for (ctx, field_def) in fields.iter_mut().zip(defs_iter) {
        match field_def.field_type {
            FieldType::Relationship => {
                if let Some(ref rc) = field_def.relationship {
                    // Fetch documents from related collection for options
                    if let Some(related_def) = reg.get_collection(&rc.collection) {
                        let title_field = related_def.title_field().map(|s| s.to_string());
                        let find_query = query::FindQuery::default();
                        if let Ok(docs) = query::find(&conn, &rc.collection, related_def, &find_query, rel_locale_ctx.as_ref()) {
                            if rc.has_many {
                                // Get selected IDs from hydrated document
                                let selected_ids: std::collections::HashSet<String> = match doc_fields.get(&field_def.name) {
                                    Some(serde_json::Value::Array(arr)) => {
                                        arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect()
                                    }
                                    _ => std::collections::HashSet::new(),
                                };
                                let options: Vec<_> = docs.iter().map(|doc| {
                                    let label = title_field.as_ref()
                                        .and_then(|f| doc.get_str(f))
                                        .unwrap_or(&doc.id);
                                    serde_json::json!({
                                        "value": doc.id,
                                        "label": label,
                                        "selected": selected_ids.contains(&doc.id),
                                    })
                                }).collect();
                                ctx["relationship_options"] = serde_json::json!(options);
                            } else {
                                // Has-one: current value from context
                                let current_value = ctx.get("value")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let options: Vec<_> = docs.iter().map(|doc| {
                                    let label = title_field.as_ref()
                                        .and_then(|f| doc.get_str(f))
                                        .unwrap_or(&doc.id);
                                    serde_json::json!({
                                        "value": doc.id,
                                        "label": label,
                                        "selected": doc.id == current_value,
                                    })
                                }).collect();
                                ctx["relationship_options"] = serde_json::json!(options);
                            }
                        }
                    }
                }
            }
            FieldType::Array => {
                // Populate rows from hydrated document data
                let locale_locked = non_default_locale && !field_def.localized;
                let rows: Vec<serde_json::Value> = match doc_fields.get(&field_def.name) {
                    Some(serde_json::Value::Array(arr)) => {
                        arr.iter().enumerate().map(|(idx, row)| {
                            let row_obj = row.as_object();
                            let sub_values: Vec<_> = field_def.fields.iter().map(|sf| {
                                let raw_value = row_obj.and_then(|m| m.get(&sf.name));
                                build_enriched_sub_field_context(
                                    sf, raw_value, &field_def.name, idx,
                                    locale_locked, non_default_locale, 1,
                                )
                            }).collect();
                            serde_json::json!({
                                "index": idx,
                                "sub_fields": sub_values,
                            })
                        }).collect()
                    }
                    _ => Vec::new(),
                };
                ctx["row_count"] = serde_json::json!(rows.len());
                ctx["rows"] = serde_json::json!(rows);
            }
            FieldType::Upload => {
                // Upload is a has-one relationship to an upload collection
                if let Some(ref rc) = field_def.relationship {
                    if let Some(related_def) = reg.get_collection(&rc.collection) {
                        let title_field = related_def.title_field().map(|s| s.to_string());
                        let admin_thumbnail = related_def.upload.as_ref()
                            .and_then(|u| u.admin_thumbnail.as_ref().cloned());
                        let find_query = query::FindQuery::default();
                        if let Ok(mut docs) = query::find(&conn, &rc.collection, related_def, &find_query, rel_locale_ctx.as_ref()) {
                            // Assemble sizes for thumbnail lookup
                            if let Some(ref upload_config) = related_def.upload {
                                if upload_config.enabled {
                                    for doc in &mut docs {
                                        upload::assemble_sizes_object(doc, upload_config);
                                    }
                                }
                            }

                            let current_value = ctx.get("value")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();

                            let mut selected_preview_url = None;
                            let mut selected_filename = None;

                            let options: Vec<_> = docs.iter().map(|doc| {
                                let label = doc.get_str("filename")
                                    .or_else(|| title_field.as_ref().and_then(|f| doc.get_str(f)))
                                    .unwrap_or(&doc.id);
                                let mime = doc.get_str("mime_type").unwrap_or("");
                                let is_image = mime.starts_with("image/");

                                // Get thumbnail URL
                                let thumb_url = if is_image {
                                    admin_thumbnail.as_ref()
                                        .and_then(|thumb_name| {
                                            doc.fields.get("sizes")
                                                .and_then(|v| v.get(thumb_name))
                                                .and_then(|v| v.get("url"))
                                                .and_then(|v| v.as_str())
                                                .map(|s| s.to_string())
                                        })
                                        .or_else(|| doc.get_str("url").map(|s| s.to_string()))
                                } else {
                                    None
                                };

                                let is_selected = doc.id == current_value;
                                if is_selected {
                                    selected_preview_url = thumb_url.clone();
                                    selected_filename = Some(label.to_string());
                                }

                                let mut opt = serde_json::json!({
                                    "value": doc.id,
                                    "label": label,
                                    "selected": is_selected,
                                });
                                if let Some(ref url) = thumb_url {
                                    opt["thumbnail_url"] = serde_json::json!(url);
                                }
                                if is_image {
                                    opt["is_image"] = serde_json::json!(true);
                                }
                                opt["filename"] = serde_json::json!(label);
                                opt
                            }).collect();
                            ctx["relationship_options"] = serde_json::json!(options);
                            ctx["relationship_collection"] = serde_json::json!(rc.collection);

                            if let Some(url) = selected_preview_url {
                                ctx["selected_preview_url"] = serde_json::json!(url);
                            }
                            if let Some(fname) = selected_filename {
                                ctx["selected_filename"] = serde_json::json!(fname);
                            }
                        }
                    }
                }
            }
            FieldType::Blocks => {
                // Populate rows from hydrated document data
                let locale_locked = non_default_locale && !field_def.localized;
                let rows: Vec<serde_json::Value> = match doc_fields.get(&field_def.name) {
                    Some(serde_json::Value::Array(arr)) => {
                        arr.iter().enumerate().map(|(idx, row)| {
                            let row_obj = row.as_object();
                            let block_type = row_obj
                                .and_then(|m| m.get("_block_type"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("unknown");
                            let block_label = field_def.blocks.iter()
                                .find(|bd| bd.block_type == block_type)
                                .and_then(|bd| bd.label.as_ref().map(|ls| ls.resolve_default()))
                                .unwrap_or(block_type);
                            let block_def = field_def.blocks.iter()
                                .find(|bd| bd.block_type == block_type);
                            let sub_values: Vec<_> = block_def
                                .map(|bd| bd.fields.iter().map(|sf| {
                                    let raw_value = row_obj.and_then(|m| m.get(&sf.name));
                                    build_enriched_sub_field_context(
                                        sf, raw_value, &field_def.name, idx,
                                        locale_locked, non_default_locale, 1,
                                    )
                                }).collect())
                                .unwrap_or_default();
                            serde_json::json!({
                                "index": idx,
                                "_block_type": block_type,
                                "block_label": block_label,
                                "sub_fields": sub_values,
                            })
                        }).collect()
                    }
                    _ => Vec::new(),
                };
                ctx["row_count"] = serde_json::json!(rows.len());
                ctx["rows"] = serde_json::json!(rows);
            }
            _ => {}
        }
    }
}

/// Render a 403 Forbidden page with the given message.
pub(super) fn forbidden(state: &AdminState, message: &str) -> (StatusCode, Html<String>) {
    let data = ContextBuilder::new(state, None)
        .page(PageType::Error403, "Forbidden")
        .set("message", serde_json::Value::String(message.to_string()))
        .build();
    let data = state.hook_runner.run_before_render(data);
    let html = match state.render("errors/403", &data) {
        Ok(html) => Html(html),
        Err(_) => Html(format!("<h1>403 Forbidden</h1><p>{}</p>", message)),
    };
    (StatusCode::FORBIDDEN, html)
}

/// Create a redirect response to the given URL.
pub(super) fn redirect_response(url: &str) -> axum::response::Response {
    Redirect::to(url).into_response()
}

/// Render a template and set the X-Crap-Toast header for client-side notifications.
pub(super) fn html_with_toast(state: &AdminState, template: &str, data: &serde_json::Value, toast: &str) -> axum::response::Response {
    match state.render(template, data) {
        Ok(html) => {
            let mut resp = Html(html).into_response();
            if let Ok(val) = toast.parse() {
                resp.headers_mut().insert("X-Crap-Toast", val);
            }
            resp
        }
        Err(e) => Html(format!("<h1>Template Error</h1><pre>{}</pre>", e)).into_response(),
    }
}

/// Render a template, falling back to a plain error page on failure.
pub(super) fn render_or_error(state: &AdminState, template: &str, data: &serde_json::Value) -> Html<String> {
    match state.render(template, data) {
        Ok(html) => Html(html),
        Err(e) => Html(format!("<h1>Template Error</h1><pre>{}</pre>", e)),
    }
}

/// Render a 404 Not Found page with the given message.
pub(super) fn not_found(state: &AdminState, message: &str) -> (StatusCode, Html<String>) {
    let data = ContextBuilder::new(state, None)
        .page(PageType::Error404, "Not Found")
        .set("message", serde_json::Value::String(message.to_string()))
        .build();
    let data = state.hook_runner.run_before_render(data);
    let html = match state.render("errors/404", &data) {
        Ok(html) => Html(html),
        Err(_) => Html(format!("<h1>404</h1><p>{}</p>", message)),
    };
    (StatusCode::NOT_FOUND, html)
}

/// Render a 500 Internal Server Error page with the given message.
pub(super) fn server_error(state: &AdminState, message: &str) -> (StatusCode, Html<String>) {
    let data = ContextBuilder::new(state, None)
        .page(PageType::Error500, "Server Error")
        .set("message", serde_json::Value::String(message.to_string()))
        .build();
    let data = state.hook_runner.run_before_render(data);
    let html = match state.render("errors/500", &data) {
        Ok(html) => Html(html),
        Err(_) => Html(format!("<h1>500</h1><p>{}</p>", message)),
    };
    (StatusCode::INTERNAL_SERVER_ERROR, html)
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- auto_label_from_name tests ---

    #[test]
    fn auto_label_underscore_separated() {
        assert_eq!(auto_label_from_name("my_field"), "My Field");
    }

    #[test]
    fn auto_label_single_word() {
        assert_eq!(auto_label_from_name("title"), "Title");
    }

    #[test]
    fn auto_label_empty_string() {
        assert_eq!(auto_label_from_name(""), "");
    }

    #[test]
    fn auto_label_multiple_words() {
        assert_eq!(auto_label_from_name("created_at"), "Created At");
    }

    #[test]
    fn auto_label_double_underscore() {
        assert_eq!(auto_label_from_name("seo__title"), "Seo  Title");
    }

    // --- strip_denied_fields tests ---

    #[test]
    fn strip_denied_fields_removes_specified_keys() {
        let mut fields = HashMap::new();
        fields.insert("title".to_string(), serde_json::json!("Hello"));
        fields.insert("secret".to_string(), serde_json::json!("hidden"));
        fields.insert("body".to_string(), serde_json::json!("content"));

        strip_denied_fields(&mut fields, &["secret".to_string()]);

        assert_eq!(fields.len(), 2);
        assert!(fields.contains_key("title"));
        assert!(fields.contains_key("body"));
        assert!(!fields.contains_key("secret"));
    }

    #[test]
    fn strip_denied_fields_empty_denied_list() {
        let mut fields = HashMap::new();
        fields.insert("title".to_string(), serde_json::json!("Hello"));
        fields.insert("body".to_string(), serde_json::json!("content"));

        strip_denied_fields(&mut fields, &[]);

        assert_eq!(fields.len(), 2);
        assert!(fields.contains_key("title"));
        assert!(fields.contains_key("body"));
    }

    #[test]
    fn strip_denied_fields_empty_fields_map() {
        let mut fields: HashMap<String, serde_json::Value> = HashMap::new();
        strip_denied_fields(&mut fields, &["secret".to_string()]);
        assert!(fields.is_empty());
    }

    #[test]
    fn strip_denied_fields_nonexistent_key() {
        let mut fields = HashMap::new();
        fields.insert("title".to_string(), serde_json::json!("Hello"));

        strip_denied_fields(&mut fields, &["nonexistent".to_string()]);

        assert_eq!(fields.len(), 1);
        assert!(fields.contains_key("title"));
    }

    // --- build_field_contexts: array/block sub-field enrichment tests ---

    use crate::core::field::{FieldDefinition, FieldAdmin, FieldHooks, FieldAccess, SelectOption, LocalizedString, BlockDefinition};

    fn make_field(name: &str, ft: FieldType) -> FieldDefinition {
        FieldDefinition {
            name: name.to_string(),
            field_type: ft,
            required: false,
            unique: false,
            validate: None,
            default_value: None,
            options: Vec::new(),
            admin: FieldAdmin::default(),
            hooks: FieldHooks::default(),
            access: FieldAccess::default(),
            relationship: None,
            fields: Vec::new(),
            blocks: Vec::new(),
            localized: false,
            picker_appearance: None,
        }
    }

    #[test]
    fn build_field_contexts_array_sub_fields_include_type_and_label() {
        let mut arr_field = make_field("items", FieldType::Array);
        arr_field.fields = vec![
            make_field("title", FieldType::Text),
            make_field("body", FieldType::Richtext),
        ];
        let fields = vec![arr_field];
        let values = HashMap::new();
        let errors = HashMap::new();
        let result = build_field_contexts(&fields, &values, &errors, false, false);
        assert_eq!(result.len(), 1);
        let sub_fields = result[0]["sub_fields"].as_array().unwrap();
        assert_eq!(sub_fields.len(), 2);
        assert_eq!(sub_fields[0]["field_type"], "text");
        assert_eq!(sub_fields[0]["label"], "Title");
        assert_eq!(sub_fields[1]["field_type"], "richtext");
        assert_eq!(sub_fields[1]["label"], "Body");
    }

    #[test]
    fn build_field_contexts_array_select_sub_field_includes_options() {
        let mut select_sf = make_field("status", FieldType::Select);
        select_sf.options = vec![
            SelectOption { label: LocalizedString::Plain("Draft".to_string()), value: "draft".to_string() },
            SelectOption { label: LocalizedString::Plain("Published".to_string()), value: "published".to_string() },
        ];
        let mut arr_field = make_field("items", FieldType::Array);
        arr_field.fields = vec![select_sf];
        let fields = vec![arr_field];
        let values = HashMap::new();
        let errors = HashMap::new();
        let result = build_field_contexts(&fields, &values, &errors, false, false);
        let sub_fields = result[0]["sub_fields"].as_array().unwrap();
        let opts = sub_fields[0]["options"].as_array().unwrap();
        assert_eq!(opts.len(), 2);
        assert_eq!(opts[0]["value"], "draft");
        assert_eq!(opts[1]["value"], "published");
    }

    #[test]
    fn build_field_contexts_blocks_sub_fields_include_type_and_label() {
        let mut blocks_field = make_field("content", FieldType::Blocks);
        blocks_field.blocks = vec![BlockDefinition {
            block_type: "rich".to_string(),
            label: Some(LocalizedString::Plain("Rich Text".to_string())),
            fields: vec![
                make_field("heading", FieldType::Text),
                make_field("body", FieldType::Richtext),
            ],
        }];
        let fields = vec![blocks_field];
        let values = HashMap::new();
        let errors = HashMap::new();
        let result = build_field_contexts(&fields, &values, &errors, false, false);
        let block_defs = result[0]["block_definitions"].as_array().unwrap();
        assert_eq!(block_defs.len(), 1);
        let block_fields = block_defs[0]["fields"].as_array().unwrap();
        assert_eq!(block_fields.len(), 2);
        assert_eq!(block_fields[0]["field_type"], "text");
        assert_eq!(block_fields[0]["label"], "Heading");
        assert_eq!(block_fields[1]["field_type"], "richtext");
        assert_eq!(block_fields[1]["label"], "Body");
    }

    #[test]
    fn build_field_contexts_blocks_select_sub_field_includes_options() {
        let mut select_sf = make_field("align", FieldType::Select);
        select_sf.options = vec![
            SelectOption { label: LocalizedString::Plain("Left".to_string()), value: "left".to_string() },
            SelectOption { label: LocalizedString::Plain("Center".to_string()), value: "center".to_string() },
        ];
        let mut blocks_field = make_field("layout", FieldType::Blocks);
        blocks_field.blocks = vec![BlockDefinition {
            block_type: "section".to_string(),
            label: None,
            fields: vec![select_sf],
        }];
        let fields = vec![blocks_field];
        let values = HashMap::new();
        let errors = HashMap::new();
        let result = build_field_contexts(&fields, &values, &errors, false, false);
        let block_defs = result[0]["block_definitions"].as_array().unwrap();
        let block_fields = block_defs[0]["fields"].as_array().unwrap();
        let opts = block_fields[0]["options"].as_array().unwrap();
        assert_eq!(opts.len(), 2);
        assert_eq!(opts[0]["value"], "left");
        assert_eq!(opts[1]["value"], "center");
    }

    // --- build_field_contexts: date field tests ---

    #[test]
    fn build_field_contexts_date_default_day_only() {
        let date_field = make_field("published_at", FieldType::Date);
        let fields = vec![date_field];
        let mut values = HashMap::new();
        values.insert("published_at".to_string(), "2026-01-15T12:00:00.000Z".to_string());
        let errors = HashMap::new();
        let result = build_field_contexts(&fields, &values, &errors, false, false);
        assert_eq!(result[0]["picker_appearance"], "dayOnly");
        assert_eq!(result[0]["date_only_value"], "2026-01-15");
    }

    #[test]
    fn build_field_contexts_date_day_and_time() {
        let mut date_field = make_field("event_at", FieldType::Date);
        date_field.picker_appearance = Some("dayAndTime".to_string());
        let fields = vec![date_field];
        let mut values = HashMap::new();
        values.insert("event_at".to_string(), "2026-01-15T09:30:00.000Z".to_string());
        let errors = HashMap::new();
        let result = build_field_contexts(&fields, &values, &errors, false, false);
        assert_eq!(result[0]["picker_appearance"], "dayAndTime");
        assert_eq!(result[0]["datetime_local_value"], "2026-01-15T09:30");
    }

    #[test]
    fn build_field_contexts_date_time_only() {
        let mut date_field = make_field("reminder", FieldType::Date);
        date_field.picker_appearance = Some("timeOnly".to_string());
        let fields = vec![date_field];
        let mut values = HashMap::new();
        values.insert("reminder".to_string(), "14:30".to_string());
        let errors = HashMap::new();
        let result = build_field_contexts(&fields, &values, &errors, false, false);
        assert_eq!(result[0]["picker_appearance"], "timeOnly");
        assert_eq!(result[0]["value"], "14:30");
    }

    #[test]
    fn build_field_contexts_date_month_only() {
        let mut date_field = make_field("birth_month", FieldType::Date);
        date_field.picker_appearance = Some("monthOnly".to_string());
        let fields = vec![date_field];
        let mut values = HashMap::new();
        values.insert("birth_month".to_string(), "2026-01".to_string());
        let errors = HashMap::new();
        let result = build_field_contexts(&fields, &values, &errors, false, false);
        assert_eq!(result[0]["picker_appearance"], "monthOnly");
        assert_eq!(result[0]["value"], "2026-01");
    }

    // --- safe_template_id tests ---

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
        assert_eq!(safe_template_id("content[__INDEX__][items]"), "content-__INDEX__-items");
    }

    // --- Recursive build_field_contexts tests (nested composites) ---

    #[test]
    fn build_field_contexts_array_has_template_id() {
        let mut arr_field = make_field("items", FieldType::Array);
        arr_field.fields = vec![make_field("title", FieldType::Text)];
        let fields = vec![arr_field];
        let result = build_field_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
        assert_eq!(result[0]["template_id"], "items");
    }

    #[test]
    fn build_field_contexts_blocks_has_template_id() {
        let mut blocks_field = make_field("content", FieldType::Blocks);
        blocks_field.blocks = vec![BlockDefinition {
            block_type: "text".to_string(),
            label: None,
            fields: vec![make_field("body", FieldType::Text)],
        }];
        let fields = vec![blocks_field];
        let result = build_field_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
        assert_eq!(result[0]["template_id"], "content");
    }

    #[test]
    fn build_field_contexts_array_sub_fields_have_indexed_names() {
        let mut arr_field = make_field("slides", FieldType::Array);
        arr_field.fields = vec![
            make_field("title", FieldType::Text),
            make_field("body", FieldType::Textarea),
        ];
        let fields = vec![arr_field];
        let result = build_field_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
        let sub_fields = result[0]["sub_fields"].as_array().unwrap();
        // Sub-fields in the template context should have __INDEX__ placeholder names
        assert_eq!(sub_fields[0]["name"], "slides[__INDEX__][title]");
        assert_eq!(sub_fields[1]["name"], "slides[__INDEX__][body]");
    }

    #[test]
    fn build_field_contexts_nested_array_in_blocks() {
        // blocks field with a block that contains an array sub-field
        let mut inner_array = make_field("images", FieldType::Array);
        inner_array.fields = vec![
            make_field("url", FieldType::Text),
            make_field("caption", FieldType::Text),
        ];
        let mut blocks_field = make_field("content", FieldType::Blocks);
        blocks_field.blocks = vec![BlockDefinition {
            block_type: "gallery".to_string(),
            label: Some(LocalizedString::Plain("Gallery".to_string())),
            fields: vec![
                make_field("title", FieldType::Text),
                inner_array,
            ],
        }];
        let fields = vec![blocks_field];
        let result = build_field_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);

        let block_defs = result[0]["block_definitions"].as_array().unwrap();
        assert_eq!(block_defs.len(), 1);
        let block_fields = block_defs[0]["fields"].as_array().unwrap();
        assert_eq!(block_fields.len(), 2);

        // First field is simple text
        assert_eq!(block_fields[0]["field_type"], "text");
        assert_eq!(block_fields[0]["name"], "content[__INDEX__][title]");

        // Second field is a nested array
        assert_eq!(block_fields[1]["field_type"], "array");
        assert_eq!(block_fields[1]["name"], "content[__INDEX__][images]");

        // The nested array should have its own sub_fields with double __INDEX__
        let nested_sub_fields = block_fields[1]["sub_fields"].as_array().unwrap();
        assert_eq!(nested_sub_fields.len(), 2);
        assert_eq!(nested_sub_fields[0]["name"], "content[__INDEX__][images][__INDEX__][url]");
        assert_eq!(nested_sub_fields[1]["name"], "content[__INDEX__][images][__INDEX__][caption]");

        // Nested array should have template_id
        assert!(block_fields[1]["template_id"].as_str().is_some());
    }

    #[test]
    fn build_field_contexts_nested_blocks_in_array() {
        // array field with a blocks sub-field
        let mut inner_blocks = make_field("sections", FieldType::Blocks);
        inner_blocks.blocks = vec![BlockDefinition {
            block_type: "text".to_string(),
            label: None,
            fields: vec![make_field("body", FieldType::Richtext)],
        }];
        let mut arr_field = make_field("pages", FieldType::Array);
        arr_field.fields = vec![
            make_field("title", FieldType::Text),
            inner_blocks,
        ];
        let fields = vec![arr_field];
        let result = build_field_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);

        let sub_fields = result[0]["sub_fields"].as_array().unwrap();
        assert_eq!(sub_fields.len(), 2);
        assert_eq!(sub_fields[0]["field_type"], "text");
        assert_eq!(sub_fields[1]["field_type"], "blocks");

        // Nested blocks should have block_definitions
        let nested_block_defs = sub_fields[1]["block_definitions"].as_array().unwrap();
        assert_eq!(nested_block_defs.len(), 1);
        assert_eq!(nested_block_defs[0]["block_type"], "text");

        // The nested block's fields should have proper names
        let nested_block_fields = nested_block_defs[0]["fields"].as_array().unwrap();
        assert_eq!(nested_block_fields[0]["field_type"], "richtext");
        assert_eq!(nested_block_fields[0]["name"], "pages[__INDEX__][sections][__INDEX__][body]");
    }

    #[test]
    fn build_field_contexts_nested_group_in_array() {
        // array with a group sub-field
        let mut inner_group = make_field("meta", FieldType::Group);
        inner_group.fields = vec![
            make_field("author", FieldType::Text),
            make_field("date", FieldType::Date),
        ];
        let mut arr_field = make_field("entries", FieldType::Array);
        arr_field.fields = vec![inner_group];
        let fields = vec![arr_field];
        let result = build_field_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);

        let sub_fields = result[0]["sub_fields"].as_array().unwrap();
        assert_eq!(sub_fields.len(), 1);
        assert_eq!(sub_fields[0]["field_type"], "group");

        // Group sub-fields inside array use bracketed naming
        let group_sub_fields = sub_fields[0]["sub_fields"].as_array().unwrap();
        assert_eq!(group_sub_fields.len(), 2);
        assert_eq!(group_sub_fields[0]["name"], "entries[__INDEX__][meta][author]");
        assert_eq!(group_sub_fields[1]["name"], "entries[__INDEX__][meta][date]");
    }

    #[test]
    fn build_field_contexts_nested_array_in_array() {
        // array containing an array sub-field
        let mut inner_array = make_field("tags", FieldType::Array);
        inner_array.fields = vec![make_field("name", FieldType::Text)];
        let mut outer_array = make_field("items", FieldType::Array);
        outer_array.fields = vec![
            make_field("title", FieldType::Text),
            inner_array,
        ];
        let fields = vec![outer_array];
        let result = build_field_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);

        let sub_fields = result[0]["sub_fields"].as_array().unwrap();
        assert_eq!(sub_fields[1]["field_type"], "array");

        // Nested array sub_fields have double __INDEX__
        let nested_sub = sub_fields[1]["sub_fields"].as_array().unwrap();
        assert_eq!(nested_sub[0]["name"], "items[__INDEX__][tags][__INDEX__][name]");
    }

    // --- Recursive enrichment tests (build_enriched_sub_field_context) ---

    #[test]
    fn enriched_sub_field_nested_array_populates_rows() {
        let mut inner_array = make_field("images", FieldType::Array);
        inner_array.fields = vec![
            make_field("url", FieldType::Text),
            make_field("alt", FieldType::Text),
        ];

        // Simulate hydrated data: an array with 2 rows
        let raw_value = serde_json::json!([
            {"url": "img1.jpg", "alt": "First"},
            {"url": "img2.jpg", "alt": "Second"},
        ]);

        let ctx = build_enriched_sub_field_context(
            &inner_array, Some(&raw_value), "content", 0,
            false, false, 1,
        );

        assert_eq!(ctx["field_type"], "array");
        assert_eq!(ctx["row_count"], 2);

        let rows = ctx["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 2);

        // First row sub_fields
        let row0_fields = rows[0]["sub_fields"].as_array().unwrap();
        assert_eq!(row0_fields[0]["name"], "content[0][images][0][url]");
        assert_eq!(row0_fields[0]["value"], "img1.jpg");
        assert_eq!(row0_fields[1]["name"], "content[0][images][0][alt]");
        assert_eq!(row0_fields[1]["value"], "First");

        // Second row sub_fields
        let row1_fields = rows[1]["sub_fields"].as_array().unwrap();
        assert_eq!(row1_fields[0]["value"], "img2.jpg");
        assert_eq!(row1_fields[1]["value"], "Second");

        // Template sub_fields should use __INDEX__
        let template_sub = ctx["sub_fields"].as_array().unwrap();
        assert_eq!(template_sub[0]["name"], "content[0][images][__INDEX__][url]");
    }

    #[test]
    fn enriched_sub_field_nested_blocks_populates_rows() {
        let mut inner_blocks = make_field("sections", FieldType::Blocks);
        inner_blocks.blocks = vec![BlockDefinition {
            block_type: "text".to_string(),
            label: Some(LocalizedString::Plain("Text".to_string())),
            fields: vec![make_field("body", FieldType::Richtext)],
        }];

        let raw_value = serde_json::json!([
            {"_block_type": "text", "body": "<p>Hello</p>"},
        ]);

        let ctx = build_enriched_sub_field_context(
            &inner_blocks, Some(&raw_value), "page", 2,
            false, false, 1,
        );

        assert_eq!(ctx["field_type"], "blocks");
        assert_eq!(ctx["row_count"], 1);

        let rows = ctx["rows"].as_array().unwrap();
        assert_eq!(rows[0]["_block_type"], "text");
        assert_eq!(rows[0]["block_label"], "Text");

        let sub_fields = rows[0]["sub_fields"].as_array().unwrap();
        assert_eq!(sub_fields[0]["name"], "page[2][sections][0][body]");
        assert_eq!(sub_fields[0]["value"], "<p>Hello</p>");

        // Block definitions for templates
        let block_defs = ctx["block_definitions"].as_array().unwrap();
        assert_eq!(block_defs.len(), 1);
    }

    #[test]
    fn enriched_sub_field_nested_group_populates_values() {
        let mut inner_group = make_field("meta", FieldType::Group);
        inner_group.fields = vec![
            make_field("author", FieldType::Text),
            make_field("published", FieldType::Checkbox),
        ];

        let raw_value = serde_json::json!({
            "author": "Alice",
            "published": "1",
        });

        let ctx = build_enriched_sub_field_context(
            &inner_group, Some(&raw_value), "items", 0,
            false, false, 1,
        );

        assert_eq!(ctx["field_type"], "group");
        let sub_fields = ctx["sub_fields"].as_array().unwrap();
        assert_eq!(sub_fields.len(), 2);
        assert_eq!(sub_fields[0]["name"], "items[0][meta][author]");
        assert_eq!(sub_fields[0]["value"], "Alice");
        assert_eq!(sub_fields[1]["name"], "items[0][meta][published]");
        assert_eq!(sub_fields[1]["checked"], true);
    }

    #[test]
    fn enriched_sub_field_empty_nested_array() {
        let mut inner_array = make_field("tags", FieldType::Array);
        inner_array.fields = vec![make_field("name", FieldType::Text)];

        // No data
        let ctx = build_enriched_sub_field_context(
            &inner_array, None, "items", 0,
            false, false, 1,
        );

        assert_eq!(ctx["field_type"], "array");
        assert_eq!(ctx["row_count"], 0);
        let rows = ctx["rows"].as_array().unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn enriched_sub_field_select_preserves_selected() {
        let mut select_field = make_field("status", FieldType::Select);
        select_field.options = vec![
            SelectOption { label: LocalizedString::Plain("Draft".to_string()), value: "draft".to_string() },
            SelectOption { label: LocalizedString::Plain("Published".to_string()), value: "published".to_string() },
        ];

        let raw_value = serde_json::json!("published");

        let ctx = build_enriched_sub_field_context(
            &select_field, Some(&raw_value), "items", 0,
            false, false, 1,
        );

        let opts = ctx["options"].as_array().unwrap();
        assert_eq!(opts[0]["selected"], false);
        assert_eq!(opts[1]["selected"], true);
    }

    #[test]
    fn max_depth_prevents_infinite_recursion() {
        // Build a deeply nested array structure
        fn make_nested_array(depth: usize) -> FieldDefinition {
            let mut field = make_field(&format!("level{}", depth), FieldType::Array);
            if depth < 10 {
                field.fields = vec![make_nested_array(depth + 1)];
            } else {
                field.fields = vec![make_field("leaf", FieldType::Text)];
            }
            field
        }
        let deep = make_nested_array(0);
        let fields = vec![deep];
        // This should not stack overflow — MAX_FIELD_DEPTH caps recursion
        let result = build_field_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["field_type"], "array");
    }
}
