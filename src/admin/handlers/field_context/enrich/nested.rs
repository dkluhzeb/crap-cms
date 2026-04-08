//! Builds enriched sub-field contexts for array/blocks rows and recursively
//! enriches nested relationship/upload fields with DB-fetched options.

use serde_json::{Value, json};

use super::enrich_types::build_upload_item;
use crate::{
    admin::handlers::{
        field_context::{
            MAX_FIELD_DEPTH, collect_node_attr_errors,
            enrich::{SubFieldOpts, field_types},
        },
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

/// Build the indexed form name for a sub-field within an array/blocks row.
///
/// Layout wrappers are transparent — they use `parent[idx]` without appending the field name.
/// Leaf fields use `parent[idx][field_name]`.
fn sub_field_indexed_name(sf: &FieldDefinition, parent_name: &str, idx: usize) -> String {
    if matches!(
        sf.field_type,
        FieldType::Tabs | FieldType::Row | FieldType::Collapsible
    ) {
        format!("{}[{}]", parent_name, idx)
    } else {
        format!("{}[{}][{}]", parent_name, idx, sf.name)
    }
}

/// Stringify a raw JSON value for a sub-field context.
///
/// Scalar types get their string representation; composite types return empty string
/// since their structure is handled recursively.
fn stringify_sub_field_value(raw_value: Option<&Value>, sf: &FieldDefinition) -> String {
    raw_value
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
        .unwrap_or_default()
}

/// Build the base JSON context for a sub-field (before type-specific dispatch).
fn build_sub_field_base(
    sf: &FieldDefinition,
    indexed_name: &str,
    val: &str,
    opts: &SubFieldOpts,
) -> Value {
    let sf_label = sf
        .admin
        .label
        .as_ref()
        .map(|ls| ls.resolve_default().to_string())
        .unwrap_or_else(|| auto_label_from_name(&sf.name));

    let mut ctx = json!({
        "name": indexed_name,
        "field_type": sf.field_type.as_str(),
        "label": sf_label,
        "value": val,
        "required": sf.required,
        "readonly": sf.admin.readonly || opts.locale_locked,
        "locale_locked": opts.locale_locked,
        "placeholder": sf.admin.placeholder.as_ref().map(|ls| ls.resolve_default()),
        "description": sf.admin.description.as_ref().map(|ls| ls.resolve_default()),
    });

    if let Some(err) = opts.errors.get(indexed_name) {
        ctx["error"] = json!(err);
    }

    ctx
}

/// Enrich a Richtext sub-field context with format, features, nodes, and attr errors.
fn enrich_sub_richtext(
    sub_ctx: &mut Value,
    sf: &FieldDefinition,
    indexed_name: &str,
    errors: &std::collections::HashMap<String, String>,
) {
    sub_ctx["resizable"] = json!(sf.admin.resizable);

    if !sf.admin.features.is_empty() {
        sub_ctx["features"] = json!(sf.admin.features);
    }

    sub_ctx["richtext_format"] = json!(sf.admin.richtext_format.as_deref().unwrap_or("html"));

    if !sf.admin.nodes.is_empty() {
        sub_ctx["_node_names"] = json!(sf.admin.nodes);
    }

    if sub_ctx.get("error").is_none_or(|v| v.is_null())
        && let Some(node_err) = collect_node_attr_errors(errors, indexed_name)
    {
        sub_ctx["error"] = json!(node_err);
    }
}

/// Dispatch type-specific enrichment for a sub-field context.
fn dispatch_sub_field_type(
    sub_ctx: &mut Value,
    sf: &FieldDefinition,
    val: &str,
    raw_value: Option<&Value>,
    indexed_name: &str,
    opts: &SubFieldOpts,
) {
    match &sf.field_type {
        FieldType::Checkbox => field_types::sub_checkbox(sub_ctx, val),
        FieldType::Select | FieldType::Radio => field_types::sub_select_radio(sub_ctx, sf, val),
        FieldType::Date => field_types::sub_date(sub_ctx, sf, val, ""),
        FieldType::Relationship => field_types::sub_relationship(sub_ctx, sf),
        FieldType::Upload => field_types::sub_upload(sub_ctx, sf),
        FieldType::Array => field_types::sub_array(sub_ctx, sf, raw_value, indexed_name, opts),
        FieldType::Blocks => field_types::sub_blocks(sub_ctx, sf, raw_value, indexed_name, opts),
        FieldType::Group => field_types::sub_group(sub_ctx, sf, raw_value, indexed_name, opts),
        FieldType::Row | FieldType::Collapsible => {
            field_types::sub_row_collapsible(sub_ctx, sf, raw_value, indexed_name, opts)
        }
        FieldType::Tabs => field_types::sub_tabs(sub_ctx, sf, raw_value, indexed_name, opts),
        FieldType::Textarea => {
            sub_ctx["rows"] = json!(sf.admin.rows.unwrap_or(8));
            sub_ctx["resizable"] = json!(sf.admin.resizable);
        }
        FieldType::Richtext => enrich_sub_richtext(sub_ctx, sf, indexed_name, opts.errors),
        FieldType::Text | FieldType::Number if sf.has_many => {
            field_types::sub_has_many_tags(sub_ctx, val)
        }
        _ => {}
    }
}

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
    let indexed_name = sub_field_indexed_name(sf, parent_name, idx);
    let val = stringify_sub_field_value(raw_value, sf);
    let mut sub_ctx = build_sub_field_base(sf, &indexed_name, &val, opts);

    if opts.depth < MAX_FIELD_DEPTH {
        dispatch_sub_field_type(&mut sub_ctx, sf, &val, raw_value, &indexed_name, opts);
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
    let Some(ref rc) = field_def.relationship else {
        return;
    };

    // Has-many nested relationships use selected_items built by parent
    if rc.has_many {
        return;
    }

    let Some(related_def) = reg.get_collection(&rc.collection) else {
        return;
    };
    let title_field = related_def.title_field().map(|s| s.to_string());
    let current_value = ctx.get("value").and_then(|v| v.as_str()).unwrap_or("");

    if current_value.is_empty() {
        ctx["selected_items"] = json!([]);
        return;
    }

    // Internal UI enrichment — direct query for display labels, not a user-facing read.
    let item = query::find_by_id(
        conn,
        &rc.collection,
        related_def,
        current_value,
        rel_locale_ctx,
    )
    .ok()
    .flatten()
    .map(|doc| {
        let label = title_field
            .as_ref()
            .and_then(|f| doc.get_str(f))
            .unwrap_or(&doc.id)
            .to_string();
        json!({ "id": doc.id, "label": label })
    });

    ctx["selected_items"] = match item {
        Some(it) => json!([it]),
        None => json!([]),
    };
}

fn enrich_nested_upload(
    ctx: &mut Value,
    field_def: &FieldDefinition,
    conn: &dyn DbConnection,
    reg: &Registry,
    rel_locale_ctx: Option<&LocaleContext>,
) {
    let Some(ref rc) = field_def.relationship else {
        return;
    };

    // Has-many: selected_items already handled by the parent context
    if rc.has_many {
        return;
    }

    let Some(related_def) = reg.get_collection(&rc.collection) else {
        return;
    };

    let title_field = related_def.title_field().map(|s| s.to_string());
    let admin_thumbnail = related_def
        .upload
        .as_ref()
        .and_then(|u| u.admin_thumbnail.as_ref().cloned());

    let current_value = ctx.get("value").and_then(|v| v.as_str()).unwrap_or("");

    if current_value.is_empty() {
        ctx["selected_items"] = json!([]);
        return;
    }

    // Internal UI enrichment — direct query for display labels, not a user-facing read.
    let Some(mut doc) = query::find_by_id(
        conn,
        &rc.collection,
        related_def,
        current_value,
        rel_locale_ctx,
    )
    .ok()
    .flatten() else {
        ctx["selected_items"] = json!([]);
        return;
    };

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
    ctx["selected_filename"] = json!(label);

    if let Some(url) = thumb_url {
        ctx["selected_preview_url"] = json!(url);
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
