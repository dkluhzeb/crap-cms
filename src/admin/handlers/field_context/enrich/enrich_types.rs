//! Top-level field-type enrichment helpers that require DB access.
//!
//! Split from `field_types.rs` which contains the `sub_*` helpers for sub-field contexts.

use super::{enrich_nested_fields, enrich_polymorphic_selected};
use crate::{
    admin::handlers::shared::compute_row_label,
    core::{
        Document,
        field::{FieldDefinition, FieldType},
        registry::Registry,
        upload,
    },
    db::query::{self, LocaleContext},
};

use rusqlite::Connection;

use std::collections::HashMap;

use serde_json::{Value, json};

/// Enrich a top-level Relationship field context with selected items from DB.
pub(super) fn enrich_relationship(
    ctx: &mut Value,
    field_def: &FieldDefinition,
    doc_fields: &HashMap<String, Value>,
    conn: &Connection,
    reg: &Registry,
    rel_locale_ctx: Option<&LocaleContext>,
) {
    if let Some(ref rc) = field_def.relationship {
        if rc.is_polymorphic() {
            let selected_items = enrich_polymorphic_selected(
                rc,
                &field_def.name,
                doc_fields,
                reg,
                conn,
                rel_locale_ctx,
            );

            ctx["selected_items"] = json!(selected_items);
        } else if let Some(related_def) = reg.get_collection(&rc.collection) {
            let title_field = related_def.title_field().map(|s| s.to_string());

            if rc.has_many {
                let selected_ids: Vec<String> = match doc_fields.get(&field_def.name) {
                    Some(Value::Array(arr)) => arr
                        .iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect(),
                    _ => Vec::new(),
                };

                let selected_items: Vec<_> = selected_ids
                    .iter()
                    .filter_map(|id| {
                        query::find_by_id(conn, &rc.collection, related_def, id, rel_locale_ctx)
                            .ok()
                            .flatten()
                            .map(|doc| {
                                let label = title_field
                                    .as_ref()
                                    .and_then(|f| doc.get_str(f))
                                    .unwrap_or(&doc.id)
                                    .to_string();
                                json!({ "id": doc.id, "label": label })
                            })
                    })
                    .collect();

                ctx["selected_items"] = json!(selected_items);
            } else {
                let current_value = ctx
                    .get("value")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                if !current_value.is_empty() {
                    if let Ok(Some(doc)) = query::find_by_id(
                        conn,
                        &rc.collection,
                        related_def,
                        &current_value,
                        rel_locale_ctx,
                    ) {
                        let label = title_field
                            .as_ref()
                            .and_then(|f| doc.get_str(f))
                            .unwrap_or(&doc.id)
                            .to_string();
                        ctx["selected_items"] = json!([{ "id": doc.id, "label": label }]);
                    } else {
                        ctx["selected_items"] = json!([]);
                    }
                } else {
                    ctx["selected_items"] = json!([]);
                }
            }
        }
    }
}

/// Enrich a top-level Array field context with rows from hydrated document data.
pub(super) fn enrich_array(
    ctx: &mut Value,
    field_def: &FieldDefinition,
    doc_fields: &HashMap<String, Value>,
    enrich: &super::EnrichCtx,
) {
    let state = enrich.state;
    let non_default_locale = enrich.non_default_locale;
    let errors = enrich.errors;
    let conn = enrich.conn;
    let reg = enrich.reg;
    let rel_locale_ctx = enrich.rel_locale_ctx;
    let locale_locked = non_default_locale && !field_def.localized;
    let rows: Vec<Value> = match doc_fields.get(&field_def.name) {
        Some(Value::Array(arr)) => arr
            .iter()
            .enumerate()
            .map(|(idx, row)| {
                let row_obj = row.as_object();
                let sub_values: Vec<_> = field_def
                    .fields
                    .iter()
                    .map(|sf| {
                        let raw_value = if matches!(
                            sf.field_type,
                            FieldType::Tabs | FieldType::Row | FieldType::Collapsible
                        ) {
                            Some(row)
                        } else {
                            row_obj.and_then(|m| m.get(&sf.name))
                        };

                        let sub_opts = super::SubFieldOpts::builder(errors)
                            .locale_locked(locale_locked)
                            .non_default_locale(non_default_locale)
                            .depth(1)
                            .build();
                        super::build_enriched_sub_field_context(
                            sf,
                            raw_value,
                            &field_def.name,
                            idx,
                            &sub_opts,
                        )
                    })
                    .collect();

                let row_has_errors = sub_values
                    .iter()
                    .any(|sf_ctx| sf_ctx.get("error").is_some());

                let mut row_json = json!({
                    "index": idx,
                    "sub_fields": sub_values,
                });

                if row_has_errors {
                    row_json["has_errors"] = json!(true);
                }

                if let Some(label) =
                    compute_row_label(&field_def.admin, None, row_obj, &state.hook_runner)
                {
                    row_json["custom_label"] = json!(label);
                }

                row_json
            })
            .collect(),
        _ => Vec::new(),
    };

    ctx["row_count"] = json!(rows.len());
    ctx["rows"] = json!(rows);

    // Enrich Upload/Relationship sub-fields within each row
    if let Some(rows_arr) = ctx.get_mut("rows").and_then(|v| v.as_array_mut()) {
        for row_ctx in rows_arr.iter_mut() {
            if let Some(sub_arr) = row_ctx.get_mut("sub_fields").and_then(|v| v.as_array_mut()) {
                enrich_nested_fields(sub_arr, &field_def.fields, conn, reg, rel_locale_ctx);
            }
        }
    }

    // Enrich the <template> sub-fields so new rows added via JS have upload/relationship options
    if let Some(sub_arr) = ctx.get_mut("sub_fields").and_then(|v| v.as_array_mut()) {
        enrich_nested_fields(sub_arr, &field_def.fields, conn, reg, rel_locale_ctx);
    }
}

/// Enrich a top-level Upload field context with selected items from DB.
pub(super) fn enrich_upload(
    ctx: &mut Value,
    field_def: &FieldDefinition,
    doc_fields: &HashMap<String, Value>,
    conn: &Connection,
    reg: &Registry,
    rel_locale_ctx: Option<&LocaleContext>,
) {
    if let Some(ref rc) = field_def.relationship
        && let Some(related_def) = reg.get_collection(&rc.collection)
    {
        let title_field = related_def.title_field().map(|s| s.to_string());
        let admin_thumbnail = related_def
            .upload
            .as_ref()
            .and_then(|u| u.admin_thumbnail.as_ref().cloned());

        if rc.has_many {
            let selected_ids: Vec<String> = match doc_fields.get(&field_def.name) {
                Some(Value::Array(arr)) => arr
                    .iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect(),
                _ => Vec::new(),
            };

            let selected_items: Vec<_> = selected_ids
                .iter()
                .filter_map(|id| {
                    query::find_by_id(conn, &rc.collection, related_def, id, rel_locale_ctx)
                        .ok()
                        .flatten()
                        .map(|mut doc| {
                            if let Some(ref uc) = related_def.upload
                                && uc.enabled
                            {
                                upload::assemble_sizes_object(&mut doc, uc);
                            }
                            build_upload_item(&doc, &title_field, &admin_thumbnail, false)
                        })
                })
                .collect();

            ctx["selected_items"] = json!(selected_items);
        } else {
            let current_value = ctx
                .get("value")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            if !current_value.is_empty() {
                if let Ok(Some(mut doc)) = query::find_by_id(
                    conn,
                    &rc.collection,
                    related_def,
                    &current_value,
                    rel_locale_ctx,
                ) {
                    if let Some(ref uc) = related_def.upload
                        && uc.enabled
                    {
                        upload::assemble_sizes_object(&mut doc, uc);
                    }

                    let item = build_upload_item(&doc, &title_field, &admin_thumbnail, true);
                    let label = item["label"].as_str().unwrap_or("").to_string();
                    let thumb_url = item
                        .get("thumbnail_url")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());

                    ctx["selected_items"] = json!([item]);

                    if let Some(url) = thumb_url {
                        ctx["selected_preview_url"] = json!(url);
                    }

                    ctx["selected_filename"] = json!(label);
                } else {
                    ctx["selected_items"] = json!([]);
                }
            } else {
                ctx["selected_items"] = json!([]);
            }
        }
    }
}

/// Build a JSON item for an upload document (shared by has-one and has-many).
fn build_upload_item(
    doc: &Document,
    title_field: &Option<String>,
    admin_thumbnail: &Option<String>,
    include_filename: bool,
) -> Value {
    let label = doc
        .get_str("filename")
        .or_else(|| title_field.as_ref().and_then(|f| doc.get_str(f)))
        .unwrap_or(&doc.id)
        .to_string();
    let mime = doc.get_str("mime_type").unwrap_or("").to_string();
    let is_image = mime.starts_with("image/");
    let thumb_url = if is_image {
        admin_thumbnail
            .as_ref()
            .and_then(|thumb_name| {
                doc.fields
                    .get("sizes")
                    .and_then(|v| v.get(thumb_name))
                    .and_then(|v| v.get("url"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            })
            .or_else(|| doc.get_str("url").map(|s| s.to_string()))
    } else {
        None
    };

    let mut item = json!({ "id": doc.id, "label": label });

    if let Some(ref url) = thumb_url {
        item["thumbnail_url"] = json!(url);
    }

    if is_image {
        item["is_image"] = json!(true);
    }

    if include_filename {
        item["filename"] = json!(label);
    }

    item
}

/// Enrich a top-level Blocks field context with rows from hydrated document data.
pub(super) fn enrich_blocks(
    ctx: &mut Value,
    field_def: &FieldDefinition,
    doc_fields: &HashMap<String, Value>,
    enrich: &super::EnrichCtx,
) {
    let state = enrich.state;
    let non_default_locale = enrich.non_default_locale;
    let errors = enrich.errors;
    let conn = enrich.conn;
    let reg = enrich.reg;
    let rel_locale_ctx = enrich.rel_locale_ctx;
    let locale_locked = non_default_locale && !field_def.localized;
    let rows: Vec<Value> = match doc_fields.get(&field_def.name) {
        Some(Value::Array(arr)) => arr
            .iter()
            .enumerate()
            .map(|(idx, row)| {
                let row_obj = row.as_object();
                let block_type = row_obj
                    .and_then(|m| m.get("_block_type"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                let block_label = field_def
                    .blocks
                    .iter()
                    .find(|bd| bd.block_type == block_type)
                    .and_then(|bd| bd.label.as_ref().map(|ls| ls.resolve_default()))
                    .unwrap_or(block_type);
                let block_def = field_def
                    .blocks
                    .iter()
                    .find(|bd| bd.block_type == block_type);
                let block_label_field = block_def.and_then(|bd| bd.label_field.as_deref());
                let sub_values: Vec<_> = block_def
                    .map(|bd| {
                        bd.fields
                            .iter()
                            .map(|sf| {
                                let raw_value = if matches!(
                                    sf.field_type,
                                    FieldType::Tabs | FieldType::Row | FieldType::Collapsible
                                ) {
                                    Some(row)
                                } else {
                                    row_obj.and_then(|m| m.get(&sf.name))
                                };

                                let sub_opts = super::SubFieldOpts::builder(errors)
                                    .locale_locked(locale_locked)
                                    .non_default_locale(non_default_locale)
                                    .depth(1)
                                    .build();
                                super::build_enriched_sub_field_context(
                                    sf,
                                    raw_value,
                                    &field_def.name,
                                    idx,
                                    &sub_opts,
                                )
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let row_has_errors = sub_values
                    .iter()
                    .any(|sf_ctx| sf_ctx.get("error").is_some());
                let mut row_json = json!({
                    "index": idx,
                    "_block_type": block_type,
                    "block_label": block_label,
                    "sub_fields": sub_values,
                });

                if row_has_errors {
                    row_json["has_errors"] = json!(true);
                }

                if let Some(label) = compute_row_label(
                    &field_def.admin,
                    block_label_field,
                    row_obj,
                    &state.hook_runner,
                ) {
                    row_json["custom_label"] = json!(label);
                }

                row_json
            })
            .collect(),
        _ => Vec::new(),
    };

    ctx["row_count"] = json!(rows.len());
    ctx["rows"] = json!(rows);

    // Enrich Upload/Relationship sub-fields within each block row
    if let Some(rows_arr) = ctx.get_mut("rows").and_then(|v| v.as_array_mut()) {
        for row_ctx in rows_arr.iter_mut() {
            let block_type = row_ctx
                .get("_block_type")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            if let Some(block_def) = field_def
                .blocks
                .iter()
                .find(|bd| bd.block_type == block_type)
                && let Some(sub_arr) = row_ctx.get_mut("sub_fields").and_then(|v| v.as_array_mut())
            {
                enrich_nested_fields(sub_arr, &block_def.fields, conn, reg, rel_locale_ctx);
            }
        }
    }
    // Enrich Upload/Relationship sub-fields within block definition templates
    if let Some(defs_arr) = ctx
        .get_mut("block_definitions")
        .and_then(|v| v.as_array_mut())
    {
        for (def_ctx, block_def) in defs_arr.iter_mut().zip(field_def.blocks.iter()) {
            if let Some(sub_arr) = def_ctx.get_mut("fields").and_then(|v| v.as_array_mut()) {
                enrich_nested_fields(sub_arr, &block_def.fields, conn, reg, rel_locale_ctx);
            }
        }
    }
}

/// Enrich a top-level Join field context with reverse-lookup items from DB.
pub(super) fn enrich_join(
    ctx: &mut Value,
    field_def: &FieldDefinition,
    conn: &Connection,
    reg: &Registry,
    rel_locale_ctx: Option<&LocaleContext>,
    doc_id: Option<&str>,
) {
    if let Some(ref jc) = field_def.join
        && let Some(doc_id_str) = doc_id
        && let Some(target_def) = reg.get_collection(&jc.collection)
    {
        let title_field = target_def.title_field().map(|s| s.to_string());

        let mut fq = query::FindQuery::new();
        fq.filters = vec![query::FilterClause::Single(query::Filter {
            field: jc.on.clone(),
            op: query::FilterOp::Equals(doc_id_str.to_string()),
        })];

        if let Ok(docs) = query::find(conn, &jc.collection, target_def, &fq, rel_locale_ctx) {
            let items: Vec<_> = docs
                .iter()
                .map(|doc| {
                    let label = title_field
                        .as_ref()
                        .and_then(|f| doc.get_str(f))
                        .unwrap_or(&doc.id)
                        .to_string();
                    json!({ "id": doc.id, "label": label })
                })
                .collect();

            ctx["join_items"] = json!(items);
            ctx["join_count"] = json!(items.len());
        }
    }
}

/// Enrich a top-level Richtext field context with custom node definitions from registry.
pub(super) fn enrich_richtext(ctx: &mut Value, reg: &Registry) {
    if let Some(node_names) = ctx.get("_node_names").cloned() {
        if let Some(names) = node_names.as_array() {
            let node_defs: Vec<_> = names
                .iter()
                .filter_map(|n| n.as_str())
                .filter_map(|name| reg.get_richtext_node(name))
                .map(|def| {
                    json!({
                        "name": def.name,
                        "label": def.label,
                        "inline": def.inline,
                        "attrs": def.attrs.iter().map(|a| {
                            let mut attr = json!({
                                "name": a.name,
                                "type": a.attr_type.as_str(),
                                "label": a.label,
                                "required": a.required,
                            });

                            if let Some(ref dv) = a.default_value {
                                attr["default"] = dv.clone();
                            }

                            if !a.options.is_empty() {
                                attr["options"] = json!(
                                    a.options.iter().map(|o| json!({
                                        "label": o.label.resolve_default(),
                                        "value": o.value,
                                    })).collect::<Vec<_>>()
                                );
                            }
                            attr
                        }).collect::<Vec<_>>(),
                    })
                })
                .collect();

            if !node_defs.is_empty() {
                ctx["custom_nodes"] = json!(node_defs);
            }
        }
        if let Some(obj) = ctx.as_object_mut() {
            obj.remove("_node_names");
        }
    }
}
