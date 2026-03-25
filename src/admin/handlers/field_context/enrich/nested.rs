//! Builds enriched sub-field contexts for array/blocks rows and recursively
//! enriches nested relationship/upload fields with DB-fetched options.

use crate::{
    admin::handlers::{
        field_context::{MAX_FIELD_DEPTH, collect_node_attr_errors},
        shared::auto_label_from_name,
    },
    core::{
        Registry,
        field::{FieldDefinition, FieldType},
        upload,
    },
    db::{
        DbConnection,
        query::{self, LocaleContext},
    },
};

use serde_json::{Value, json};

use super::{SubFieldOpts, field_types};

/// Build an enriched sub-field context for a single field within an array/blocks row.
///
/// - `sf`: the sub-field definition
/// - `raw_value`: the raw JSON value for this sub-field from the hydrated document
/// - `parent_name`: the parent field's name (e.g. "content")
/// - `idx`: the row index within the parent
/// - `opts`: locale/depth/error options
pub fn build_enriched_sub_field_context(
    sf: &FieldDefinition,
    raw_value: Option<&Value>,
    parent_name: &str,
    idx: usize,
    opts: &SubFieldOpts,
) -> Value {
    let locale_locked = opts.locale_locked;
    let depth = opts.depth;
    let errors = opts.errors;
    let indexed_name = if matches!(
        sf.field_type,
        FieldType::Tabs | FieldType::Row | FieldType::Collapsible
    ) {
        format!("{}[{}]", parent_name, idx) // transparent — layout wrappers don't add their name
    } else {
        format!("{}[{}][{}]", parent_name, idx, sf.name)
    };

    // For scalar types, stringify the value. For composites, keep structured.
    let val = raw_value
        .map(|v| match v {
            Value::String(s) => s.clone(),
            Value::Null => String::new(),
            other => match sf.field_type {
                FieldType::Array
                | FieldType::Blocks
                | FieldType::Group
                | FieldType::Row
                | FieldType::Collapsible
                | FieldType::Tabs => String::new(),
                _ => other.to_string(),
            },
        })
        .unwrap_or_default();

    let sf_label = sf
        .admin
        .label
        .as_ref()
        .map(|ls| ls.resolve_default().to_string())
        .unwrap_or_else(|| auto_label_from_name(&sf.name));

    let mut sub_ctx = json!({
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

    if let Some(err) = errors.get(&indexed_name) {
        sub_ctx["error"] = json!(err);
    }

    if depth >= MAX_FIELD_DEPTH {
        return sub_ctx;
    }

    match &sf.field_type {
        FieldType::Checkbox => field_types::sub_checkbox(&mut sub_ctx, &val),
        FieldType::Select | FieldType::Radio => {
            field_types::sub_select_radio(&mut sub_ctx, sf, &val)
        }
        FieldType::Date => field_types::sub_date(&mut sub_ctx, sf, &val),
        FieldType::Relationship => field_types::sub_relationship(&mut sub_ctx, sf),
        FieldType::Upload => field_types::sub_upload(&mut sub_ctx, sf),
        FieldType::Array => {
            field_types::sub_array(&mut sub_ctx, sf, raw_value, &indexed_name, opts)
        }
        FieldType::Blocks => {
            field_types::sub_blocks(&mut sub_ctx, sf, raw_value, &indexed_name, opts)
        }
        FieldType::Group => {
            field_types::sub_group(&mut sub_ctx, sf, raw_value, &indexed_name, opts)
        }
        FieldType::Row | FieldType::Collapsible => {
            field_types::sub_row_collapsible(&mut sub_ctx, sf, raw_value, &indexed_name, opts)
        }
        FieldType::Tabs => field_types::sub_tabs(&mut sub_ctx, sf, raw_value, &indexed_name, opts),
        FieldType::Textarea => {
            sub_ctx["rows"] = json!(sf.admin.rows.unwrap_or(8));
            sub_ctx["resizable"] = json!(sf.admin.resizable);
        }
        FieldType::Richtext => {
            sub_ctx["resizable"] = json!(sf.admin.resizable);

            if !sf.admin.features.is_empty() {
                sub_ctx["features"] = json!(sf.admin.features);
            }

            let fmt = sf.admin.richtext_format.as_deref().unwrap_or("html");

            sub_ctx["richtext_format"] = json!(fmt);

            if !sf.admin.nodes.is_empty() {
                sub_ctx["_node_names"] = json!(sf.admin.nodes);
            }

            // Attach node attr validation errors (e.g. content[0][body][cta#0].text)
            if sub_ctx.get("error").is_none_or(|v| v.is_null())
                && let Some(node_err) = collect_node_attr_errors(errors, &indexed_name)
            {
                sub_ctx["error"] = json!(node_err);
            }
        }
        FieldType::Text | FieldType::Number if sf.has_many => {
            field_types::sub_has_many_tags(&mut sub_ctx, &val)
        }
        _ => {}
    }

    sub_ctx
}

/// Recursively enrich Upload and Relationship sub-field contexts with options from the database.
/// Called for sub-fields inside layout containers (Row, Collapsible, Tabs, Group) and
/// composite fields (Array, Blocks) that can't be enriched during initial context building.
pub fn enrich_nested_fields(
    sub_fields: &mut [Value],
    field_defs: &[FieldDefinition],
    conn: &dyn DbConnection,
    reg: &Registry,
    rel_locale_ctx: Option<&LocaleContext>,
) {
    for (ctx, field_def) in sub_fields.iter_mut().zip(field_defs.iter()) {
        match field_def.field_type {
            FieldType::Relationship => {
                enrich_nested_relationship(ctx, field_def, conn, reg, rel_locale_ctx);
            }
            FieldType::Upload => {
                enrich_nested_upload(ctx, field_def, conn, reg, rel_locale_ctx);
            }
            FieldType::Row | FieldType::Collapsible | FieldType::Group => {
                if let Some(sub_arr) = ctx.get_mut("sub_fields").and_then(|v| v.as_array_mut()) {
                    enrich_nested_fields(sub_arr, &field_def.fields, conn, reg, rel_locale_ctx);
                }
            }
            FieldType::Tabs => {
                if let Some(tabs_arr) = ctx.get_mut("tabs").and_then(|v| v.as_array_mut()) {
                    for (tab_ctx, tab_def) in tabs_arr.iter_mut().zip(field_def.tabs.iter()) {
                        if let Some(sub_arr) =
                            tab_ctx.get_mut("sub_fields").and_then(|v| v.as_array_mut())
                        {
                            enrich_nested_fields(
                                sub_arr,
                                &tab_def.fields,
                                conn,
                                reg,
                                rel_locale_ctx,
                            );
                        }
                    }
                }
            }
            FieldType::Array => {
                enrich_nested_array(ctx, field_def, conn, reg, rel_locale_ctx);
            }
            FieldType::Blocks => {
                enrich_nested_blocks(ctx, field_def, conn, reg, rel_locale_ctx);
            }
            _ => {}
        }
    }
}

fn enrich_nested_relationship(
    ctx: &mut Value,
    field_def: &FieldDefinition,
    conn: &dyn DbConnection,
    reg: &Registry,
    rel_locale_ctx: Option<&LocaleContext>,
) {
    if let Some(ref rc) = field_def.relationship
        && let Some(related_def) = reg.get_collection(&rc.collection)
    {
        let title_field = related_def.title_field().map(|s| s.to_string());
        if rc.has_many {
            // Has-many nested relationships use selected_items built by parent
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

fn enrich_nested_upload(
    ctx: &mut Value,
    field_def: &FieldDefinition,
    conn: &dyn DbConnection,
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
            // Has-many: selected_items already handled by the parent context
        } else {
            // Has-one upload: fetch only the selected doc via search widget
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

                    item["filename"] = json!(label);
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

fn enrich_nested_array(
    ctx: &mut Value,
    field_def: &FieldDefinition,
    conn: &dyn DbConnection,
    reg: &Registry,
    rel_locale_ctx: Option<&LocaleContext>,
) {
    // Recurse into array rows' sub-fields
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

fn enrich_nested_blocks(
    ctx: &mut Value,
    field_def: &FieldDefinition,
    conn: &dyn DbConnection,
    reg: &Registry,
    rel_locale_ctx: Option<&LocaleContext>,
) {
    // Recurse into block rows' sub-fields, matching each row's block type
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

    // Enrich block definition templates so new block rows have upload/relationship options
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
