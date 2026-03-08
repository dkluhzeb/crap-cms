//! Per-field-type enrichment helpers extracted from the dispatch loops in mod.rs.

use std::collections::HashMap;
use crate::core::field::FieldType;
use crate::admin::handlers::field_context::{safe_template_id, count_errors_in_fields};
use crate::admin::handlers::shared::auto_label_from_name;
use crate::admin::handlers::field_context::builder::{build_single_field_context, apply_field_type_extras};

// ── build_enriched_sub_field_context helpers ─────────────────────────

/// Enrich a Checkbox sub-field context.
pub(super) fn sub_checkbox(sub_ctx: &mut serde_json::Value, val: &str) {
    let checked = matches!(val, "1" | "true" | "on" | "yes");
    sub_ctx["checked"] = serde_json::json!(checked);
}

/// Enrich a Select/Radio sub-field context.
pub(super) fn sub_select_radio(
    sub_ctx: &mut serde_json::Value,
    sf: &crate::core::field::FieldDefinition,
    val: &str,
) {
    if sf.has_many {
        let selected_values: std::collections::HashSet<String> =
            serde_json::from_str(val).unwrap_or_default();
        let options: Vec<_> = sf.options.iter().map(|opt| {
            serde_json::json!({
                "label": opt.label.resolve_default(),
                "value": opt.value,
                "selected": selected_values.contains(&opt.value),
            })
        }).collect();
        sub_ctx["options"] = serde_json::json!(options);
        sub_ctx["has_many"] = serde_json::json!(true);
    } else {
        let options: Vec<_> = sf.options.iter().map(|opt| {
            serde_json::json!({
                "label": opt.label.resolve_default(),
                "value": opt.value,
                "selected": opt.value == val,
            })
        }).collect();
        sub_ctx["options"] = serde_json::json!(options);
    }
}

/// Enrich a Date sub-field context.
pub(super) fn sub_date(
    sub_ctx: &mut serde_json::Value,
    sf: &crate::core::field::FieldDefinition,
    val: &str,
) {
    let appearance = sf.picker_appearance.as_deref().unwrap_or("dayOnly");
    sub_ctx["picker_appearance"] = serde_json::json!(appearance);
    match appearance {
        "dayOnly" => {
            let date_val = if val.len() >= 10 { &val[..10] } else { val };
            sub_ctx["date_only_value"] = serde_json::json!(date_val);
        }
        "dayAndTime" => {
            let dt_val = if val.len() >= 16 { &val[..16] } else { val };
            sub_ctx["datetime_local_value"] = serde_json::json!(dt_val);
        }
        _ => {}
    }
}

/// Enrich a Relationship sub-field context.
pub(super) fn sub_relationship(
    sub_ctx: &mut serde_json::Value,
    sf: &crate::core::field::FieldDefinition,
) {
    if let Some(ref rc) = sf.relationship {
        sub_ctx["relationship_collection"] = serde_json::json!(rc.collection);
        sub_ctx["has_many"] = serde_json::json!(rc.has_many);
        if rc.is_polymorphic() {
            sub_ctx["polymorphic"] = serde_json::json!(true);
            sub_ctx["collections"] = serde_json::json!(rc.polymorphic);
        }
    }
    if let Some(ref p) = sf.admin.picker {
        sub_ctx["picker"] = serde_json::json!(p);
    }
}

/// Enrich an Upload sub-field context.
pub(super) fn sub_upload(
    sub_ctx: &mut serde_json::Value,
    sf: &crate::core::field::FieldDefinition,
) {
    if let Some(ref rc) = sf.relationship {
        sub_ctx["relationship_collection"] = serde_json::json!(rc.collection);
        if rc.has_many {
            sub_ctx["has_many"] = serde_json::json!(true);
        }
    }
    let picker = sf.admin.picker.as_deref().unwrap_or("drawer");
    if picker != "none" {
        sub_ctx["picker"] = serde_json::json!(picker);
    }
}

/// Enrich a nested Array sub-field context (within another Array/Blocks row).
pub(super) fn sub_array(
    sub_ctx: &mut serde_json::Value,
    sf: &crate::core::field::FieldDefinition,
    raw_value: Option<&serde_json::Value>,
    indexed_name: &str,
    locale_locked: bool,
    non_default_locale: bool,
    depth: usize,
    errors: &HashMap<String, String>,
) {
    let nested_rows: Vec<serde_json::Value> = match raw_value {
        Some(serde_json::Value::Array(arr)) => {
            arr.iter().enumerate().map(|(nested_idx, nested_row)| {
                let nested_row_obj = nested_row.as_object();
                let nested_sub_values: Vec<_> = sf.fields.iter().map(|nested_sf| {
                    let nested_raw = if matches!(nested_sf.field_type,
                        FieldType::Tabs | FieldType::Row | FieldType::Collapsible)
                    {
                        Some(nested_row)
                    } else {
                        nested_row_obj.and_then(|m| m.get(&nested_sf.name))
                    };
                    super::build_enriched_sub_field_context(
                        nested_sf, nested_raw, indexed_name, nested_idx,
                        locale_locked, non_default_locale, depth + 1, errors,
                    )
                }).collect();
                let row_has_errors = nested_sub_values.iter()
                    .any(|sf_ctx| sf_ctx.get("error").is_some());
                let mut row_json = serde_json::json!({
                    "index": nested_idx,
                    "sub_fields": nested_sub_values,
                });
                if row_has_errors {
                    row_json["has_errors"] = serde_json::json!(true);
                }
                row_json
            }).collect()
        }
        _ => Vec::new(),
    };
    let template_prefix = format!("{}[__INDEX__]", indexed_name);
    let template_sub_fields: Vec<_> = sf.fields.iter().map(|nested_sf| {
        build_single_field_context(nested_sf, &HashMap::new(), &HashMap::new(), &template_prefix, non_default_locale, depth + 1)
    }).collect();
    sub_ctx["sub_fields"] = serde_json::json!(template_sub_fields);
    sub_ctx["rows"] = serde_json::json!(nested_rows);
    sub_ctx["row_count"] = serde_json::json!(nested_rows.len());
    sub_ctx["template_id"] = serde_json::json!(safe_template_id(indexed_name));
    if let Some(max) = sf.max_rows {
        sub_ctx["max_rows"] = serde_json::json!(max);
    }
    if let Some(min) = sf.min_rows {
        sub_ctx["min_rows"] = serde_json::json!(min);
    }
    sub_ctx["init_collapsed"] = serde_json::json!(sf.admin.collapsed);
    if let Some(ref ls) = sf.admin.labels_singular {
        sub_ctx["add_label"] = serde_json::json!(ls.resolve_default());
    }
}

/// Enrich a nested Blocks sub-field context (within another Array/Blocks row).
pub(super) fn sub_blocks(
    sub_ctx: &mut serde_json::Value,
    sf: &crate::core::field::FieldDefinition,
    raw_value: Option<&serde_json::Value>,
    indexed_name: &str,
    locale_locked: bool,
    non_default_locale: bool,
    depth: usize,
    errors: &HashMap<String, String>,
) {
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
                        let nested_raw = if matches!(nested_sf.field_type,
                            FieldType::Tabs | FieldType::Row | FieldType::Collapsible)
                        {
                            Some(nested_row)
                        } else {
                            nested_row_obj.and_then(|m| m.get(&nested_sf.name))
                        };
                        super::build_enriched_sub_field_context(
                            nested_sf, nested_raw, indexed_name, nested_idx,
                            locale_locked, non_default_locale, depth + 1, errors,
                        )
                    }).collect())
                    .unwrap_or_default();
                let row_has_errors = nested_sub_values.iter()
                    .any(|sf_ctx| sf_ctx.get("error").is_some());
                let mut row_json = serde_json::json!({
                    "index": nested_idx,
                    "_block_type": block_type,
                    "block_label": block_label,
                    "sub_fields": nested_sub_values,
                });
                if row_has_errors {
                    row_json["has_errors"] = serde_json::json!(true);
                }
                row_json
            }).collect()
        }
        _ => Vec::new(),
    };
    let block_defs: Vec<_> = sf.blocks.iter().map(|bd| {
        let template_prefix = format!("{}[__INDEX__]", indexed_name);
        let block_fields: Vec<_> = bd.fields.iter().map(|nested_sf| {
            build_single_field_context(nested_sf, &HashMap::new(), &HashMap::new(), &template_prefix, non_default_locale, depth + 1)
        }).collect();
        let mut def = serde_json::json!({
            "block_type": bd.block_type,
            "label": bd.label.as_ref().map(|ls| ls.resolve_default()).unwrap_or(&bd.block_type),
            "fields": block_fields,
        });
        if let Some(ref lf) = bd.label_field {
            def["label_field"] = serde_json::json!(lf);
        }
        if let Some(ref g) = bd.group {
            def["group"] = serde_json::json!(g);
        }
        if let Some(ref url) = bd.image_url {
            def["image_url"] = serde_json::json!(url);
        }
        def
    }).collect();
    sub_ctx["block_definitions"] = serde_json::json!(block_defs);
    sub_ctx["rows"] = serde_json::json!(nested_rows);
    sub_ctx["row_count"] = serde_json::json!(nested_rows.len());
    sub_ctx["template_id"] = serde_json::json!(safe_template_id(indexed_name));
    if let Some(ref lf) = sf.admin.label_field {
        sub_ctx["label_field"] = serde_json::json!(lf);
    }
    if let Some(max) = sf.max_rows {
        sub_ctx["max_rows"] = serde_json::json!(max);
    }
    if let Some(min) = sf.min_rows {
        sub_ctx["min_rows"] = serde_json::json!(min);
    }
    sub_ctx["init_collapsed"] = serde_json::json!(sf.admin.collapsed);
    if let Some(ref ls) = sf.admin.labels_singular {
        sub_ctx["add_label"] = serde_json::json!(ls.resolve_default());
    }
}

/// Enrich a nested Group sub-field context.
pub(super) fn sub_group(
    sub_ctx: &mut serde_json::Value,
    sf: &crate::core::field::FieldDefinition,
    raw_value: Option<&serde_json::Value>,
    indexed_name: &str,
    locale_locked: bool,
    non_default_locale: bool,
    depth: usize,
) {
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
    sub_ctx["collapsed"] = serde_json::json!(sf.admin.collapsed);
}

/// Enrich a nested Row/Collapsible sub-field context.
pub(super) fn sub_row_collapsible(
    sub_ctx: &mut serde_json::Value,
    sf: &crate::core::field::FieldDefinition,
    raw_value: Option<&serde_json::Value>,
    indexed_name: &str,
    locale_locked: bool,
    non_default_locale: bool,
    depth: usize,
    errors: &HashMap<String, String>,
) {
    let nested_sub_fields = super::build_enriched_children_from_data(
        &sf.fields, raw_value, indexed_name,
        locale_locked, non_default_locale, depth + 1, errors,
    );
    sub_ctx["sub_fields"] = serde_json::json!(nested_sub_fields);
    if sf.field_type == FieldType::Collapsible {
        sub_ctx["collapsed"] = serde_json::json!(sf.admin.collapsed);
    }
}

/// Enrich a nested Tabs sub-field context.
pub(super) fn sub_tabs(
    sub_ctx: &mut serde_json::Value,
    sf: &crate::core::field::FieldDefinition,
    raw_value: Option<&serde_json::Value>,
    indexed_name: &str,
    locale_locked: bool,
    non_default_locale: bool,
    depth: usize,
    errors: &HashMap<String, String>,
) {
    let tabs_ctx: Vec<_> = sf.tabs.iter().map(|tab| {
        let tab_sub_fields = super::build_enriched_children_from_data(
            &tab.fields, raw_value, indexed_name,
            locale_locked, non_default_locale, depth + 1, errors,
        );
        let error_count = count_errors_in_fields(&tab_sub_fields);
        let mut tab_ctx = serde_json::json!({
            "label": &tab.label,
            "sub_fields": tab_sub_fields,
        });
        if error_count > 0 {
            tab_ctx["error_count"] = serde_json::json!(error_count);
        }
        if let Some(ref desc) = tab.description {
            tab_ctx["description"] = serde_json::json!(desc);
        }
        tab_ctx
    }).collect();
    sub_ctx["tabs"] = serde_json::json!(tabs_ctx);
}

/// Enrich a Text/Number has_many sub-field context (tag input).
pub(super) fn sub_has_many_tags(sub_ctx: &mut serde_json::Value, val: &str) {
    let tags: Vec<String> = serde_json::from_str(val).unwrap_or_default();
    sub_ctx["has_many"] = serde_json::json!(true);
    sub_ctx["tags"] = serde_json::json!(tags);
    sub_ctx["value"] = serde_json::json!(tags.join(","));
}

// ── enrich_field_contexts helpers ────────────────────────────────────

/// Enrich a top-level Relationship field context with selected items from DB.
pub(super) fn enrich_relationship(
    ctx: &mut serde_json::Value,
    field_def: &crate::core::field::FieldDefinition,
    doc_fields: &HashMap<String, serde_json::Value>,
    conn: &rusqlite::Connection,
    reg: &crate::core::Registry,
    rel_locale_ctx: Option<&crate::db::query::LocaleContext>,
) {
    use crate::db::query;

    if let Some(ref rc) = field_def.relationship {
        if rc.is_polymorphic() {
            let selected_items = super::enrich_polymorphic_selected(
                rc, &field_def.name, doc_fields, reg, conn, rel_locale_ctx,
            );
            ctx["selected_items"] = serde_json::json!(selected_items);
        } else if let Some(related_def) = reg.get_collection(&rc.collection) {
            let title_field = related_def.title_field().map(|s| s.to_string());
            if rc.has_many {
                let selected_ids: Vec<String> = match doc_fields.get(&field_def.name) {
                    Some(serde_json::Value::Array(arr)) => {
                        arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect()
                    }
                    _ => Vec::new(),
                };
                let selected_items: Vec<_> = selected_ids.iter().filter_map(|id| {
                    query::find_by_id(conn, &rc.collection, related_def, id, rel_locale_ctx)
                        .ok()
                        .flatten()
                        .map(|doc| {
                            let label = title_field.as_ref()
                                .and_then(|f| doc.get_str(f))
                                .unwrap_or(&doc.id)
                                .to_string();
                            serde_json::json!({ "id": doc.id, "label": label })
                        })
                }).collect();
                ctx["selected_items"] = serde_json::json!(selected_items);
            } else {
                let current_value = ctx.get("value")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if !current_value.is_empty() {
                    if let Ok(Some(doc)) = query::find_by_id(conn, &rc.collection, related_def, &current_value, rel_locale_ctx) {
                        let label = title_field.as_ref()
                            .and_then(|f| doc.get_str(f))
                            .unwrap_or(&doc.id)
                            .to_string();
                        ctx["selected_items"] = serde_json::json!([{ "id": doc.id, "label": label }]);
                    } else {
                        ctx["selected_items"] = serde_json::json!([]);
                    }
                } else {
                    ctx["selected_items"] = serde_json::json!([]);
                }
            }
        }
    }
}

/// Enrich a top-level Array field context with rows from hydrated document data.
pub(super) fn enrich_array(
    ctx: &mut serde_json::Value,
    field_def: &crate::core::field::FieldDefinition,
    doc_fields: &HashMap<String, serde_json::Value>,
    state: &crate::admin::AdminState,
    non_default_locale: bool,
    errors: &HashMap<String, String>,
    conn: &rusqlite::Connection,
    reg: &crate::core::Registry,
    rel_locale_ctx: Option<&crate::db::query::LocaleContext>,
) {
    let locale_locked = non_default_locale && !field_def.localized;
    let rows: Vec<serde_json::Value> = match doc_fields.get(&field_def.name) {
        Some(serde_json::Value::Array(arr)) => {
            arr.iter().enumerate().map(|(idx, row)| {
                let row_obj = row.as_object();
                let sub_values: Vec<_> = field_def.fields.iter().map(|sf| {
                    let raw_value = if matches!(sf.field_type,
                        FieldType::Tabs | FieldType::Row | FieldType::Collapsible)
                    {
                        Some(row)
                    } else {
                        row_obj.and_then(|m| m.get(&sf.name))
                    };
                    super::build_enriched_sub_field_context(
                        sf, raw_value, &field_def.name, idx,
                        locale_locked, non_default_locale, 1, errors,
                    )
                }).collect();
                let row_has_errors = sub_values.iter()
                    .any(|sf_ctx| sf_ctx.get("error").is_some());
                let mut row_json = serde_json::json!({
                    "index": idx,
                    "sub_fields": sub_values,
                });
                if row_has_errors {
                    row_json["has_errors"] = serde_json::json!(true);
                }
                use crate::admin::handlers::shared::compute_row_label;
                if let Some(label) = compute_row_label(
                    &field_def.admin, None, row_obj, &state.hook_runner,
                ) {
                    row_json["custom_label"] = serde_json::json!(label);
                }
                row_json
            }).collect()
        }
        _ => Vec::new(),
    };
    ctx["row_count"] = serde_json::json!(rows.len());
    ctx["rows"] = serde_json::json!(rows);
    // Enrich Upload/Relationship sub-fields within each row
    if let Some(rows_arr) = ctx.get_mut("rows").and_then(|v| v.as_array_mut()) {
        for row_ctx in rows_arr.iter_mut() {
            if let Some(sub_arr) = row_ctx.get_mut("sub_fields").and_then(|v| v.as_array_mut()) {
                super::enrich_nested_fields(sub_arr, &field_def.fields, conn, reg, rel_locale_ctx);
            }
        }
    }
    // Enrich the <template> sub-fields so new rows added via JS have upload/relationship options
    if let Some(sub_arr) = ctx.get_mut("sub_fields").and_then(|v| v.as_array_mut()) {
        super::enrich_nested_fields(sub_arr, &field_def.fields, conn, reg, rel_locale_ctx);
    }
}

/// Enrich a top-level Upload field context with selected items from DB.
pub(super) fn enrich_upload(
    ctx: &mut serde_json::Value,
    field_def: &crate::core::field::FieldDefinition,
    doc_fields: &HashMap<String, serde_json::Value>,
    conn: &rusqlite::Connection,
    reg: &crate::core::Registry,
    rel_locale_ctx: Option<&crate::db::query::LocaleContext>,
) {
    use crate::core::upload;
    use crate::db::query;

    if let Some(ref rc) = field_def.relationship {
        if let Some(related_def) = reg.get_collection(&rc.collection) {
            let title_field = related_def.title_field().map(|s| s.to_string());
            let admin_thumbnail = related_def.upload.as_ref()
                .and_then(|u| u.admin_thumbnail.as_ref().cloned());

            if rc.has_many {
                let selected_ids: Vec<String> = match doc_fields.get(&field_def.name) {
                    Some(serde_json::Value::Array(arr)) => {
                        arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect()
                    }
                    _ => Vec::new(),
                };
                let selected_items: Vec<_> = selected_ids.iter().filter_map(|id| {
                    query::find_by_id(conn, &rc.collection, related_def, id, rel_locale_ctx)
                        .ok()
                        .flatten()
                        .map(|mut doc| {
                            if let Some(ref uc) = related_def.upload {
                                if uc.enabled { upload::assemble_sizes_object(&mut doc, uc); }
                            }
                            build_upload_item(&doc, &title_field, &admin_thumbnail, false)
                        })
                }).collect();
                ctx["selected_items"] = serde_json::json!(selected_items);
            } else {
                let current_value = ctx.get("value")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if !current_value.is_empty() {
                    if let Ok(Some(mut doc)) = query::find_by_id(conn, &rc.collection, related_def, &current_value, rel_locale_ctx) {
                        if let Some(ref uc) = related_def.upload {
                            if uc.enabled { upload::assemble_sizes_object(&mut doc, uc); }
                        }
                        let item = build_upload_item(&doc, &title_field, &admin_thumbnail, true);
                        let label = item["label"].as_str().unwrap_or("").to_string();
                        let thumb_url = item.get("thumbnail_url").and_then(|v| v.as_str()).map(|s| s.to_string());
                        ctx["selected_items"] = serde_json::json!([item]);
                        if let Some(url) = thumb_url {
                            ctx["selected_preview_url"] = serde_json::json!(url);
                        }
                        ctx["selected_filename"] = serde_json::json!(label);
                    } else {
                        ctx["selected_items"] = serde_json::json!([]);
                    }
                } else {
                    ctx["selected_items"] = serde_json::json!([]);
                }
            }
        }
    }
}

/// Build a JSON item for an upload document (shared by has-one and has-many).
fn build_upload_item(
    doc: &crate::core::Document,
    title_field: &Option<String>,
    admin_thumbnail: &Option<String>,
    include_filename: bool,
) -> serde_json::Value {
    let label = doc.get_str("filename")
        .or_else(|| title_field.as_ref().and_then(|f| doc.get_str(f)))
        .unwrap_or(&doc.id)
        .to_string();
    let mime = doc.get_str("mime_type").unwrap_or("").to_string();
    let is_image = mime.starts_with("image/");
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
    } else { None };
    let mut item = serde_json::json!({ "id": doc.id, "label": label });
    if let Some(ref url) = thumb_url { item["thumbnail_url"] = serde_json::json!(url); }
    if is_image { item["is_image"] = serde_json::json!(true); }
    if include_filename {
        item["filename"] = serde_json::json!(label);
    }
    item
}

/// Enrich a top-level Blocks field context with rows from hydrated document data.
pub(super) fn enrich_blocks(
    ctx: &mut serde_json::Value,
    field_def: &crate::core::field::FieldDefinition,
    doc_fields: &HashMap<String, serde_json::Value>,
    state: &crate::admin::AdminState,
    non_default_locale: bool,
    errors: &HashMap<String, String>,
    conn: &rusqlite::Connection,
    reg: &crate::core::Registry,
    rel_locale_ctx: Option<&crate::db::query::LocaleContext>,
) {
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
                let block_label_field = block_def.and_then(|bd| bd.label_field.as_deref());
                let sub_values: Vec<_> = block_def
                    .map(|bd| bd.fields.iter().map(|sf| {
                        let raw_value = if matches!(sf.field_type,
                            FieldType::Tabs | FieldType::Row | FieldType::Collapsible)
                        {
                            Some(row)
                        } else {
                            row_obj.and_then(|m| m.get(&sf.name))
                        };
                        super::build_enriched_sub_field_context(
                            sf, raw_value, &field_def.name, idx,
                            locale_locked, non_default_locale, 1, errors,
                        )
                    }).collect())
                    .unwrap_or_default();
                let row_has_errors = sub_values.iter()
                    .any(|sf_ctx| sf_ctx.get("error").is_some());
                let mut row_json = serde_json::json!({
                    "index": idx,
                    "_block_type": block_type,
                    "block_label": block_label,
                    "sub_fields": sub_values,
                });
                if row_has_errors {
                    row_json["has_errors"] = serde_json::json!(true);
                }
                use crate::admin::handlers::shared::compute_row_label;
                if let Some(label) = compute_row_label(
                    &field_def.admin, block_label_field, row_obj, &state.hook_runner,
                ) {
                    row_json["custom_label"] = serde_json::json!(label);
                }
                row_json
            }).collect()
        }
        _ => Vec::new(),
    };
    ctx["row_count"] = serde_json::json!(rows.len());
    ctx["rows"] = serde_json::json!(rows);
    // Enrich Upload/Relationship sub-fields within each block row
    if let Some(rows_arr) = ctx.get_mut("rows").and_then(|v| v.as_array_mut()) {
        for row_ctx in rows_arr.iter_mut() {
            let block_type = row_ctx.get("_block_type")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if let Some(block_def) = field_def.blocks.iter().find(|bd| bd.block_type == block_type) {
                if let Some(sub_arr) = row_ctx.get_mut("sub_fields").and_then(|v| v.as_array_mut()) {
                    super::enrich_nested_fields(sub_arr, &block_def.fields, conn, reg, rel_locale_ctx);
                }
            }
        }
    }
    // Enrich Upload/Relationship sub-fields within block definition templates
    if let Some(defs_arr) = ctx.get_mut("block_definitions").and_then(|v| v.as_array_mut()) {
        for (def_ctx, block_def) in defs_arr.iter_mut().zip(field_def.blocks.iter()) {
            if let Some(sub_arr) = def_ctx.get_mut("fields").and_then(|v| v.as_array_mut()) {
                super::enrich_nested_fields(sub_arr, &block_def.fields, conn, reg, rel_locale_ctx);
            }
        }
    }
}

/// Enrich a top-level Join field context with reverse-lookup items from DB.
pub(super) fn enrich_join(
    ctx: &mut serde_json::Value,
    field_def: &crate::core::field::FieldDefinition,
    conn: &rusqlite::Connection,
    reg: &crate::core::Registry,
    rel_locale_ctx: Option<&crate::db::query::LocaleContext>,
    doc_id: Option<&str>,
) {
    use crate::db::query;

    if let Some(ref jc) = field_def.join {
        if let Some(doc_id_str) = doc_id {
            if let Some(target_def) = reg.get_collection(&jc.collection) {
                let title_field = target_def.title_field().map(|s| s.to_string());
                let mut fq = query::FindQuery::new();
                fq.filters = vec![query::FilterClause::Single(query::Filter {
                    field: jc.on.clone(),
                    op: query::FilterOp::Equals(doc_id_str.to_string()),
                })];
                if let Ok(docs) = query::find(conn, &jc.collection, target_def, &fq, rel_locale_ctx) {
                    let items: Vec<_> = docs.iter().map(|doc| {
                        let label = title_field.as_ref()
                            .and_then(|f| doc.get_str(f))
                            .unwrap_or(&doc.id)
                            .to_string();
                        serde_json::json!({ "id": doc.id, "label": label })
                    }).collect();
                    ctx["join_items"] = serde_json::json!(items);
                    ctx["join_count"] = serde_json::json!(items.len());
                }
            }
        }
    }
}

/// Enrich a top-level Richtext field context with custom node definitions from registry.
pub(super) fn enrich_richtext(
    ctx: &mut serde_json::Value,
    reg: &crate::core::Registry,
) {
    if let Some(node_names) = ctx.get("_node_names").cloned() {
        if let Some(names) = node_names.as_array() {
            let node_defs: Vec<_> = names.iter()
                .filter_map(|n| n.as_str())
                .filter_map(|name| reg.get_richtext_node(name))
                .map(|def| serde_json::json!({
                    "name": def.name,
                    "label": def.label,
                    "inline": def.inline,
                    "attrs": def.attrs.iter().map(|a| {
                        let mut attr = serde_json::json!({
                            "name": a.name,
                            "type": a.attr_type.as_str(),
                            "label": a.label,
                            "required": a.required,
                        });
                        if let Some(ref dv) = a.default_value {
                            attr["default"] = dv.clone();
                        }
                        if !a.options.is_empty() {
                            attr["options"] = serde_json::json!(
                                a.options.iter().map(|o| serde_json::json!({
                                    "label": o.label.resolve_default(),
                                    "value": o.value,
                                })).collect::<Vec<_>>()
                            );
                        }
                        attr
                    }).collect::<Vec<_>>(),
                }))
                .collect();
            if !node_defs.is_empty() {
                ctx["custom_nodes"] = serde_json::json!(node_defs);
            }
        }
        if let Some(obj) = ctx.as_object_mut() {
            obj.remove("_node_names");
        }
    }
}
