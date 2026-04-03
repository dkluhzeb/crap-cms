//! Top-level field-type enrichment helpers that require DB access.
//!
//! Split from `field_types.rs` which contains the `sub_*` helpers for sub-field contexts.

use std::collections::HashMap;

use serde_json::{Value, json};

use crate::{
    admin::handlers::{
        field_context::{
            enrich::{
                EnrichCtx, SubFieldOpts, build_enriched_sub_field_context, enrich_nested_fields,
                enrich_polymorphic_selected,
            },
            inject_timezone_values_from_row,
        },
        shared::compute_row_label,
    },
    core::{
        Document,
        field::{FieldDefinition, FieldType, to_title_case},
        registry::Registry,
        upload,
    },
    db::{
        DbConnection,
        query::{self, LocaleContext},
    },
};

/// Extract selected IDs from a has-many field value.
fn extract_selected_ids(doc_fields: &HashMap<String, Value>, field_name: &str) -> Vec<String> {
    match doc_fields.get(field_name) {
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect(),
        _ => Vec::new(),
    }
}

/// Build a `{ id, label }` JSON item from a document.
fn doc_to_label_item(doc: &Document, title_field: &Option<String>) -> Value {
    let label = title_field
        .as_ref()
        .and_then(|f| doc.get_str(f))
        .unwrap_or(&doc.id)
        .to_string();

    json!({ "id": doc.id, "label": label })
}

/// Resolve has-many selected items by looking up each ID in the DB.
fn resolve_has_many_items(
    ids: &[String],
    collection: &str,
    related_def: &crate::core::collection::CollectionDefinition,
    title_field: &Option<String>,
    conn: &dyn DbConnection,
    rel_locale_ctx: Option<&LocaleContext>,
) -> Vec<Value> {
    ids.iter()
        .filter_map(|id| {
            query::find_by_id(conn, collection, related_def, id, rel_locale_ctx)
                .ok()
                .flatten()
                .map(|doc| doc_to_label_item(&doc, title_field))
        })
        .collect()
}

/// Resolve a has-one selected item by looking up the current value in the DB.
fn resolve_has_one_item(
    ctx: &Value,
    collection: &str,
    related_def: &crate::core::collection::CollectionDefinition,
    title_field: &Option<String>,
    conn: &dyn DbConnection,
    rel_locale_ctx: Option<&LocaleContext>,
) -> Vec<Value> {
    let current_value = ctx.get("value").and_then(|v| v.as_str()).unwrap_or("");

    if current_value.is_empty() {
        return Vec::new();
    }

    query::find_by_id(conn, collection, related_def, current_value, rel_locale_ctx)
        .ok()
        .flatten()
        .map(|doc| vec![doc_to_label_item(&doc, title_field)])
        .unwrap_or_default()
}

/// Enrich a top-level Relationship field context with selected items from DB.
pub(super) fn enrich_relationship(
    ctx: &mut Value,
    field_def: &FieldDefinition,
    doc_fields: &HashMap<String, Value>,
    conn: &dyn DbConnection,
    reg: &Registry,
    rel_locale_ctx: Option<&LocaleContext>,
) {
    let Some(ref rc) = field_def.relationship else {
        return;
    };

    if rc.is_polymorphic() {
        let items =
            enrich_polymorphic_selected(rc, &field_def.name, doc_fields, reg, conn, rel_locale_ctx);
        ctx["selected_items"] = json!(items);
        return;
    }

    let Some(related_def) = reg.get_collection(&rc.collection) else {
        return;
    };

    let title_field = related_def.title_field().map(|s| s.to_string());

    let items = if rc.has_many {
        let ids = extract_selected_ids(doc_fields, &field_def.name);
        resolve_has_many_items(
            &ids,
            &rc.collection,
            related_def,
            &title_field,
            conn,
            rel_locale_ctx,
        )
    } else {
        resolve_has_one_item(
            ctx,
            &rc.collection,
            related_def,
            &title_field,
            conn,
            rel_locale_ctx,
        )
    };

    ctx["selected_items"] = json!(items);
}

/// Extract the raw value for a sub-field from a row, handling layout wrappers transparently.
fn extract_sub_field_value<'a>(
    sf: &FieldDefinition,
    row: &'a Value,
    row_obj: Option<&'a serde_json::Map<String, Value>>,
) -> Option<&'a Value> {
    if matches!(
        sf.field_type,
        FieldType::Tabs | FieldType::Row | FieldType::Collapsible
    ) {
        Some(row)
    } else {
        row_obj.and_then(|m| m.get(&sf.name))
    }
}

/// Build sub-field contexts for a single array row.
fn build_array_row_sub_fields(
    field_def: &FieldDefinition,
    row: &Value,
    idx: usize,
    locale_locked: bool,
    enrich: &EnrichCtx,
) -> Vec<Value> {
    let row_obj = row.as_object();

    let mut sub_values: Vec<_> = field_def
        .fields
        .iter()
        .map(|sf| {
            let raw_value = extract_sub_field_value(sf, row, row_obj);

            let sub_opts = SubFieldOpts::builder(enrich.errors)
                .locale_locked(locale_locked)
                .non_default_locale(enrich.non_default_locale)
                .depth(1)
                .build();

            build_enriched_sub_field_context(sf, raw_value, &field_def.name, idx, &sub_opts)
        })
        .collect();

    inject_timezone_values_from_row(&mut sub_values, &field_def.fields, row_obj);
    sub_values
}

/// Build a single array row JSON object with index, sub_fields, errors, and custom label.
fn build_array_row(
    field_def: &FieldDefinition,
    row: &Value,
    idx: usize,
    locale_locked: bool,
    enrich: &EnrichCtx,
) -> Value {
    let sub_values = build_array_row_sub_fields(field_def, row, idx, locale_locked, enrich);
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

    if let Some(label) = compute_row_label(
        &field_def.admin,
        None,
        row.as_object(),
        &enrich.state.hook_runner,
    ) {
        row_json["custom_label"] = json!(label);
    }

    row_json
}

/// Enrich nested Upload/Relationship sub-fields in existing row and template contexts.
fn enrich_row_and_template_nested_fields(
    ctx: &mut Value,
    fields: &[FieldDefinition],
    conn: &dyn DbConnection,
    reg: &Registry,
    rel_locale_ctx: Option<&LocaleContext>,
) {
    if let Some(rows_arr) = ctx.get_mut("rows").and_then(|v| v.as_array_mut()) {
        for row_ctx in rows_arr.iter_mut() {
            if let Some(sub_arr) = row_ctx.get_mut("sub_fields").and_then(|v| v.as_array_mut()) {
                enrich_nested_fields(sub_arr, fields, conn, reg, rel_locale_ctx);
            }
        }
    }

    if let Some(sub_arr) = ctx.get_mut("sub_fields").and_then(|v| v.as_array_mut()) {
        enrich_nested_fields(sub_arr, fields, conn, reg, rel_locale_ctx);
    }
}

/// Enrich a top-level Array field context with rows from hydrated document data.
pub(super) fn enrich_array(
    ctx: &mut Value,
    field_def: &FieldDefinition,
    doc_fields: &HashMap<String, Value>,
    enrich: &EnrichCtx,
) {
    let locale_locked = enrich.non_default_locale && !field_def.localized;

    let rows: Vec<Value> = match doc_fields.get(&field_def.name) {
        Some(Value::Array(arr)) => arr
            .iter()
            .enumerate()
            .map(|(idx, row)| build_array_row(field_def, row, idx, locale_locked, enrich))
            .collect(),
        _ => Vec::new(),
    };

    ctx["row_count"] = json!(rows.len());
    ctx["rows"] = json!(rows);

    enrich_row_and_template_nested_fields(
        ctx,
        &field_def.fields,
        enrich.conn,
        enrich.reg,
        enrich.rel_locale_ctx,
    );
}

/// Assemble sizes and build an upload item for a document.
fn prepare_upload_doc(
    mut doc: Document,
    related_def: &crate::core::collection::CollectionDefinition,
    title_field: &Option<String>,
    admin_thumbnail: &Option<String>,
    include_filename: bool,
) -> Value {
    if let Some(ref uc) = related_def.upload
        && uc.enabled
    {
        upload::assemble_sizes_object(&mut doc, uc);
    }

    build_upload_item(&doc, title_field, admin_thumbnail, include_filename)
}

/// Resolve has-many upload items by looking up each ID in the DB.
fn resolve_upload_has_many(
    ids: &[String],
    collection: &str,
    related_def: &crate::core::collection::CollectionDefinition,
    title_field: &Option<String>,
    admin_thumbnail: &Option<String>,
    conn: &dyn DbConnection,
    rel_locale_ctx: Option<&LocaleContext>,
) -> Vec<Value> {
    ids.iter()
        .filter_map(|id| {
            query::find_by_id(conn, collection, related_def, id, rel_locale_ctx)
                .ok()
                .flatten()
                .map(|doc| {
                    prepare_upload_doc(doc, related_def, title_field, admin_thumbnail, false)
                })
        })
        .collect()
}

/// Resolve a has-one upload item, setting preview URL and filename on the context.
fn resolve_upload_has_one(
    ctx: &mut Value,
    collection: &str,
    related_def: &crate::core::collection::CollectionDefinition,
    title_field: &Option<String>,
    admin_thumbnail: &Option<String>,
    conn: &dyn DbConnection,
    rel_locale_ctx: Option<&LocaleContext>,
) {
    let current_value = ctx.get("value").and_then(|v| v.as_str()).unwrap_or("");

    if current_value.is_empty() {
        ctx["selected_items"] = json!([]);
        return;
    }

    let Some(doc) = query::find_by_id(conn, collection, related_def, current_value, rel_locale_ctx)
        .ok()
        .flatten()
    else {
        ctx["selected_items"] = json!([]);
        return;
    };

    let item = prepare_upload_doc(doc, related_def, title_field, admin_thumbnail, true);
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

/// Enrich a top-level Upload field context with selected items from DB.
pub(super) fn enrich_upload(
    ctx: &mut Value,
    field_def: &FieldDefinition,
    doc_fields: &HashMap<String, Value>,
    conn: &dyn DbConnection,
    reg: &Registry,
    rel_locale_ctx: Option<&LocaleContext>,
) {
    let Some(ref rc) = field_def.relationship else {
        return;
    };
    let Some(related_def) = reg.get_collection(&rc.collection) else {
        return;
    };

    let title_field = related_def.title_field().map(|s| s.to_string());
    let admin_thumbnail = related_def
        .upload
        .as_ref()
        .and_then(|u| u.admin_thumbnail.as_ref().cloned());

    if rc.has_many {
        let ids = extract_selected_ids(doc_fields, &field_def.name);
        let items = resolve_upload_has_many(
            &ids,
            &rc.collection,
            related_def,
            &title_field,
            &admin_thumbnail,
            conn,
            rel_locale_ctx,
        );
        ctx["selected_items"] = json!(items);
    } else {
        resolve_upload_has_one(
            ctx,
            &rc.collection,
            related_def,
            &title_field,
            &admin_thumbnail,
            conn,
            rel_locale_ctx,
        );
    }
}

/// Build a JSON item for an upload document (shared by has-one and has-many).
pub(super) fn build_upload_item(
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

/// Build sub-field contexts for a single blocks row.
fn build_blocks_row_sub_fields(
    block_def: &crate::core::field::BlockDefinition,
    row: &Value,
    field_name: &str,
    idx: usize,
    locale_locked: bool,
    enrich: &EnrichCtx,
) -> Vec<Value> {
    let row_obj = row.as_object();

    let mut sub_values: Vec<_> = block_def
        .fields
        .iter()
        .map(|sf| {
            let raw_value = extract_sub_field_value(sf, row, row_obj);

            let sub_opts = SubFieldOpts::builder(enrich.errors)
                .locale_locked(locale_locked)
                .non_default_locale(enrich.non_default_locale)
                .depth(1)
                .build();

            build_enriched_sub_field_context(sf, raw_value, field_name, idx, &sub_opts)
        })
        .collect();

    inject_timezone_values_from_row(&mut sub_values, &block_def.fields, row_obj);
    sub_values
}

/// Build a single blocks row JSON object.
fn build_blocks_row(
    field_def: &FieldDefinition,
    row: &Value,
    idx: usize,
    locale_locked: bool,
    enrich: &EnrichCtx,
) -> Value {
    let row_obj = row.as_object();
    let block_type = row_obj
        .and_then(|m| m.get("_block_type"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    let block_def = field_def
        .blocks
        .iter()
        .find(|bd| bd.block_type == block_type);

    let block_label = block_def
        .and_then(|bd| bd.label.as_ref().map(|ls| ls.resolve_default()))
        .unwrap_or(block_type);

    let sub_values = block_def
        .map(|bd| build_blocks_row_sub_fields(bd, row, &field_def.name, idx, locale_locked, enrich))
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

    let block_label_field = block_def.and_then(|bd| bd.label_field.as_deref());

    if let Some(label) = compute_row_label(
        &field_def.admin,
        block_label_field,
        row_obj,
        &enrich.state.hook_runner,
    ) {
        row_json["custom_label"] = json!(label);
    }

    row_json
}

/// Enrich nested sub-fields within existing block rows and block definition templates.
fn enrich_blocks_nested_fields(
    ctx: &mut Value,
    field_def: &FieldDefinition,
    conn: &dyn DbConnection,
    reg: &Registry,
    rel_locale_ctx: Option<&LocaleContext>,
) {
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

/// Enrich a top-level Blocks field context with rows from hydrated document data.
pub(super) fn enrich_blocks(
    ctx: &mut Value,
    field_def: &FieldDefinition,
    doc_fields: &HashMap<String, Value>,
    enrich: &EnrichCtx,
) {
    let locale_locked = enrich.non_default_locale && !field_def.localized;

    let rows: Vec<Value> = match doc_fields.get(&field_def.name) {
        Some(Value::Array(arr)) => arr
            .iter()
            .enumerate()
            .map(|(idx, row)| build_blocks_row(field_def, row, idx, locale_locked, enrich))
            .collect(),
        _ => Vec::new(),
    };

    ctx["row_count"] = json!(rows.len());
    ctx["rows"] = json!(rows);

    enrich_blocks_nested_fields(
        ctx,
        field_def,
        enrich.conn,
        enrich.reg,
        enrich.rel_locale_ctx,
    );
}

/// Enrich a top-level Join field context with reverse-lookup items from DB.
pub(super) fn enrich_join(
    ctx: &mut Value,
    field_def: &FieldDefinition,
    conn: &dyn DbConnection,
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

/// Build a JSON attribute object for a richtext node field definition.
fn build_node_attr(f: &FieldDefinition) -> Value {
    let label = f
        .admin
        .label
        .as_ref()
        .map(|ls| ls.resolve_default().to_string())
        .unwrap_or_else(|| to_title_case(&f.name));

    let mut attr = json!({
        "name": f.name,
        "type": f.field_type.as_str(),
        "label": label,
        "required": f.required,
    });

    if let Some(ref dv) = f.default_value {
        attr["default"] = dv.clone();
    }

    if !f.options.is_empty() {
        attr["options"] = json!(
            f.options
                .iter()
                .map(|o| json!({
                    "label": o.label.resolve_default(),
                    "value": o.value,
                }))
                .collect::<Vec<_>>()
        );
    }

    apply_node_attr_admin_hints(f, &mut attr);
    apply_node_attr_validation(f, &mut attr);
    attr
}

/// Apply admin display hints to a richtext node attribute.
fn apply_node_attr_admin_hints(f: &FieldDefinition, attr: &mut Value) {
    if let Some(ref ph) = f.admin.placeholder {
        attr["placeholder"] = json!(ph.resolve_default());
    }
    if let Some(ref desc) = f.admin.description {
        attr["description"] = json!(desc.resolve_default());
    }
    if f.admin.hidden {
        attr["hidden"] = json!(true);
    }
    if f.admin.readonly {
        attr["readonly"] = json!(true);
    }
    if let Some(ref w) = f.admin.width {
        attr["width"] = json!(w);
    }
    if let Some(ref s) = f.admin.step {
        attr["step"] = json!(s);
    }
    if let Some(rows) = f.admin.rows {
        attr["rows"] = json!(rows);
    }
    if let Some(ref lang) = f.admin.language {
        attr["language"] = json!(lang);
    }
}

/// Apply validation bounds to a richtext node attribute.
fn apply_node_attr_validation(f: &FieldDefinition, attr: &mut Value) {
    if let Some(v) = f.min {
        attr["min"] = json!(v);
    }
    if let Some(v) = f.max {
        attr["max"] = json!(v);
    }
    if let Some(v) = f.min_length {
        attr["min_length"] = json!(v);
    }
    if let Some(v) = f.max_length {
        attr["max_length"] = json!(v);
    }
    if let Some(ref d) = f.min_date {
        attr["min_date"] = json!(d);
    }
    if let Some(ref d) = f.max_date {
        attr["max_date"] = json!(d);
    }
    if let Some(ref pa) = f.picker_appearance {
        attr["picker_appearance"] = json!(pa);
    }
}

/// Enrich a top-level Richtext field context with custom node definitions from registry.
pub(super) fn enrich_richtext(ctx: &mut Value, reg: &Registry) {
    let Some(node_names) = ctx.get("_node_names").cloned() else {
        return;
    };

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
                    "attrs": def.attrs.iter().map(build_node_attr).collect::<Vec<_>>(),
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
