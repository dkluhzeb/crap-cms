//! Top-level field-type enrichment helpers that require DB access.
//!
//! Split from `field_types.rs` which contains the `sub_*` helpers for sub-field contexts.

use std::collections::HashMap;

use serde_json::Value;
use tracing::warn;

use crate::{
    admin::{
        context::field::{
            ArrayField, ArrayRow, BlockRow, BlocksField, FieldContext, JoinField, JoinItem,
            RelationshipField, RelationshipSelectedItem, RichtextField, RichtextNodeAttrCtx,
            RichtextNodeAttrOption, RichtextNodeDefCtx, UploadField,
        },
        handlers::{
            field_context::{
                enrich::{
                    EnrichCtx, SubFieldOpts, build_enriched_sub_field_context,
                    enrich_nested_fields, enrich_polymorphic_selected,
                },
                inject_lang_values_from_row, inject_timezone_values_from_row,
            },
            shared::compute_row_label,
        },
    },
    core::{
        Document,
        collection::CollectionDefinition,
        field::{BlockDefinition, FieldDefinition, FieldType, to_title_case},
        registry::Registry,
        upload,
    },
    db::{
        DbConnection,
        query::{self, LocaleContext},
    },
};

/// Extract selected IDs from a has-many field value.
///
/// Logs a warning when the stored value isn't an array. Empty / missing keys
/// are normal (no rows yet) and stay quiet; a string value here usually
/// signals a `has_many` flag that disagrees with the storage shape (data
/// migrated from has_one without a backfill, hand-edited DB row, or a
/// faulty Lua hook), which would otherwise present as an empty selector
/// without explanation.
fn extract_selected_ids(doc_fields: &HashMap<String, Value>, field_name: &str) -> Vec<String> {
    match doc_fields.get(field_name) {
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect(),
        None | Some(Value::Null) => Vec::new(),
        Some(other) => {
            let kind = match other {
                Value::String(_) => "string",
                Value::Number(_) => "number",
                Value::Bool(_) => "bool",
                Value::Object(_) => "object",
                _ => "other",
            };
            warn!(
                field = field_name,
                kind = kind,
                "has_many relationship value is not an array; selected items will be empty",
            );
            Vec::new()
        }
    }
}

/// Build a typed `{ id, label }` item from a document.
fn doc_to_label_item(doc: &Document, title_field: &Option<String>) -> RelationshipSelectedItem {
    let label = title_field
        .as_ref()
        .and_then(|f| doc.get_str(f))
        .unwrap_or(&doc.id)
        .to_string();

    RelationshipSelectedItem {
        id: doc.id.to_string(),
        label,
        ..Default::default()
    }
}

/// Resolve has-many selected items by looking up each ID in the DB.
///
/// Internal UI enrichment — direct query for display labels, not a user-facing read.
fn resolve_has_many_items(
    ids: &[String],
    collection: &str,
    related_def: &CollectionDefinition,
    title_field: &Option<String>,
    conn: &dyn DbConnection,
    rel_locale_ctx: Option<&LocaleContext>,
) -> Vec<RelationshipSelectedItem> {
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
///
/// Internal UI enrichment — direct query for display labels, not a user-facing read.
fn resolve_has_one_item(
    current_value: &str,
    collection: &str,
    related_def: &CollectionDefinition,
    title_field: &Option<String>,
    conn: &dyn DbConnection,
    rel_locale_ctx: Option<&LocaleContext>,
) -> Vec<RelationshipSelectedItem> {
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
    rf: &mut RelationshipField,
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
        rf.selected_items = Some(enrich_polymorphic_selected(
            rc,
            &field_def.name,
            doc_fields,
            reg,
            conn,
            rel_locale_ctx,
        ));
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
        // has_one expects a single string id. If the stored value is an
        // array (e.g. `has_many` flipped to `has_one` without a backfill),
        // `as_str()` silently returns None and the selector renders empty.
        // Warn so the operator sees the mismatch.
        if matches!(rf.base.value, Value::Array(_)) {
            warn!(
                field = field_def.name.as_str(),
                "has_one relationship value is an array; selector will render empty",
            );
        }
        let current_value = rf.base.value.as_str().unwrap_or("");
        resolve_has_one_item(
            current_value,
            &rc.collection,
            related_def,
            &title_field,
            conn,
            rel_locale_ctx,
        )
    };

    rf.selected_items = Some(items);
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

/// Build typed sub-field contexts for a single array row.
fn build_array_row_sub_fields(
    field_def: &FieldDefinition,
    row: &Value,
    idx: usize,
    locale_locked: bool,
    enrich: &EnrichCtx,
) -> Vec<FieldContext> {
    let row_obj = row.as_object();

    let mut sub_fields: Vec<FieldContext> = field_def
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

    inject_timezone_values_from_row(&mut sub_fields, &field_def.fields, row_obj);
    inject_lang_values_from_row(&mut sub_fields, &field_def.fields, row_obj);
    sub_fields
}

/// Build a single typed [`ArrayRow`] with index, sub_fields, errors, and custom label.
fn build_array_row(
    field_def: &FieldDefinition,
    row: &Value,
    idx: usize,
    locale_locked: bool,
    enrich: &EnrichCtx,
) -> ArrayRow {
    let sub_fields = build_array_row_sub_fields(field_def, row, idx, locale_locked, enrich);
    let row_has_errors = sub_fields.iter().any(|fc| fc.base().error.is_some());

    let custom_label = compute_row_label(
        &field_def.admin,
        None,
        row.as_object(),
        &enrich.state.hook_runner,
    );

    ArrayRow {
        index: idx,
        sub_fields,
        has_errors: if row_has_errors { Some(true) } else { None },
        custom_label,
    }
}

/// Enrich a top-level Array field context with rows from hydrated document data.
pub(super) fn enrich_array(
    af: &mut ArrayField,
    field_def: &FieldDefinition,
    doc_fields: &HashMap<String, Value>,
    enrich: &EnrichCtx,
) {
    let locale_locked = enrich.non_default_locale && !field_def.localized;

    let rows: Vec<ArrayRow> = match doc_fields.get(&field_def.name) {
        Some(Value::Array(arr)) => arr
            .iter()
            .enumerate()
            .map(|(idx, row)| build_array_row(field_def, row, idx, locale_locked, enrich))
            .collect(),
        _ => Vec::new(),
    };

    af.row_count = rows.len();
    af.rows = Some(rows);

    // Recurse into row sub_fields and template sub_fields for nested enrichment.
    if let Some(rows) = af.rows.as_mut() {
        for row in rows.iter_mut() {
            enrich_nested_fields(
                &mut row.sub_fields,
                &field_def.fields,
                enrich.conn,
                enrich.reg,
                enrich.rel_locale_ctx,
            );
        }
    }
    enrich_nested_fields(
        &mut af.sub_fields,
        &field_def.fields,
        enrich.conn,
        enrich.reg,
        enrich.rel_locale_ctx,
    );
}

/// Assemble sizes and build a typed upload item for a document.
fn prepare_upload_doc(
    mut doc: Document,
    related_def: &CollectionDefinition,
    title_field: &Option<String>,
    admin_thumbnail: &Option<String>,
    include_filename: bool,
) -> RelationshipSelectedItem {
    if let Some(ref uc) = related_def.upload
        && uc.enabled
    {
        upload::assemble_sizes_object(&mut doc, uc);
    }

    build_upload_item(&doc, title_field, admin_thumbnail, include_filename)
}

/// Resolve has-many upload items by looking up each ID in the DB.
///
/// Internal UI enrichment — direct query for display labels, not a user-facing read.
fn resolve_upload_has_many(
    ids: &[String],
    collection: &str,
    related_def: &CollectionDefinition,
    title_field: &Option<String>,
    admin_thumbnail: &Option<String>,
    conn: &dyn DbConnection,
    rel_locale_ctx: Option<&LocaleContext>,
) -> Vec<RelationshipSelectedItem> {
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

/// Resolve a has-one upload item, setting preview URL and filename on the field.
///
/// Internal UI enrichment — direct query for display labels, not a user-facing read.
fn resolve_upload_has_one(
    uf: &mut UploadField,
    collection: &str,
    related_def: &CollectionDefinition,
    title_field: &Option<String>,
    admin_thumbnail: &Option<String>,
    conn: &dyn DbConnection,
    rel_locale_ctx: Option<&LocaleContext>,
) {
    let current_value = uf.base.value.as_str().unwrap_or("");

    if current_value.is_empty() {
        uf.selected_items = Some(Vec::new());
        return;
    }

    let Some(doc) = query::find_by_id(conn, collection, related_def, current_value, rel_locale_ctx)
        .ok()
        .flatten()
    else {
        uf.selected_items = Some(Vec::new());
        return;
    };

    let item = prepare_upload_doc(doc, related_def, title_field, admin_thumbnail, true);
    let label = item.label.clone();
    let thumb_url = item.thumbnail_url.clone();

    uf.selected_items = Some(vec![item]);
    uf.selected_filename = Some(label);

    if let Some(url) = thumb_url {
        uf.selected_preview_url = Some(url);
    }
}

/// Enrich a top-level Upload field context with selected items from DB.
pub(super) fn enrich_upload(
    uf: &mut UploadField,
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
        uf.selected_items = Some(items);
    } else {
        resolve_upload_has_one(
            uf,
            &rc.collection,
            related_def,
            &title_field,
            &admin_thumbnail,
            conn,
            rel_locale_ctx,
        );
    }
}

/// Build a typed item for an upload document (shared by has-one and has-many).
pub(super) fn build_upload_item(
    doc: &Document,
    title_field: &Option<String>,
    admin_thumbnail: &Option<String>,
    include_filename: bool,
) -> RelationshipSelectedItem {
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

    RelationshipSelectedItem {
        id: doc.id.to_string(),
        label: label.clone(),
        thumbnail_url: thumb_url,
        is_image: if is_image { Some(true) } else { None },
        filename: if include_filename { Some(label) } else { None },
        ..Default::default()
    }
}

/// Build typed sub-field contexts for a single blocks row.
fn build_blocks_row_sub_fields(
    block_def: &BlockDefinition,
    row: &Value,
    field_name: &str,
    idx: usize,
    locale_locked: bool,
    enrich: &EnrichCtx,
) -> Vec<FieldContext> {
    let row_obj = row.as_object();

    let mut sub_fields: Vec<FieldContext> = block_def
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

    inject_timezone_values_from_row(&mut sub_fields, &block_def.fields, row_obj);
    inject_lang_values_from_row(&mut sub_fields, &block_def.fields, row_obj);
    sub_fields
}

/// Build a single typed [`BlockRow`].
fn build_blocks_row(
    field_def: &FieldDefinition,
    row: &Value,
    idx: usize,
    locale_locked: bool,
    enrich: &EnrichCtx,
) -> BlockRow {
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
        .and_then(|bd| bd.label.as_ref().map(|ls| ls.resolve_default().to_string()))
        .unwrap_or_else(|| block_type.to_string());

    let sub_fields = block_def
        .map(|bd| build_blocks_row_sub_fields(bd, row, &field_def.name, idx, locale_locked, enrich))
        .unwrap_or_default();

    let row_has_errors = sub_fields.iter().any(|fc| fc.base().error.is_some());

    let block_label_field = block_def.and_then(|bd| bd.label_field.as_deref());
    let custom_label = compute_row_label(
        &field_def.admin,
        block_label_field,
        row_obj,
        &enrich.state.hook_runner,
    );

    BlockRow {
        index: idx,
        block_type: block_type.to_string(),
        block_label,
        sub_fields,
        has_errors: if row_has_errors { Some(true) } else { None },
        custom_label,
    }
}

/// Enrich a top-level Blocks field context with rows from hydrated document data.
pub(super) fn enrich_blocks(
    bf: &mut BlocksField,
    field_def: &FieldDefinition,
    doc_fields: &HashMap<String, Value>,
    enrich: &EnrichCtx,
) {
    let locale_locked = enrich.non_default_locale && !field_def.localized;

    let rows: Vec<BlockRow> = match doc_fields.get(&field_def.name) {
        Some(Value::Array(arr)) => arr
            .iter()
            .enumerate()
            .map(|(idx, row)| build_blocks_row(field_def, row, idx, locale_locked, enrich))
            .collect(),
        _ => Vec::new(),
    };

    bf.row_count = rows.len();
    bf.rows = Some(rows);

    // Recurse into block rows' sub-fields, matching each row's block type.
    if let Some(rows) = bf.rows.as_mut() {
        for row in rows.iter_mut() {
            if let Some(block_def) = field_def
                .blocks
                .iter()
                .find(|bd| bd.block_type == row.block_type)
            {
                enrich_nested_fields(
                    &mut row.sub_fields,
                    &block_def.fields,
                    enrich.conn,
                    enrich.reg,
                    enrich.rel_locale_ctx,
                );
            }
        }
    }

    // Enrich block definition templates so new block rows have upload/relationship options.
    for (def_ctx, block_def) in bf.block_definitions.iter_mut().zip(field_def.blocks.iter()) {
        enrich_nested_fields(
            &mut def_ctx.fields,
            &block_def.fields,
            enrich.conn,
            enrich.reg,
            enrich.rel_locale_ctx,
        );
    }
}

/// Enrich a top-level Join field context with reverse-lookup items from DB.
pub(super) fn enrich_join(
    jf: &mut JoinField,
    field_def: &FieldDefinition,
    conn: &dyn DbConnection,
    reg: &Registry,
    rel_locale_ctx: Option<&LocaleContext>,
    doc_id: Option<&str>,
) {
    // Internal UI enrichment — direct query for display labels, not a user-facing read.
    if let Some(ref jc) = field_def.join
        && let Some(doc_id_str) = doc_id
        && let Some(target_def) = reg.get_collection(&jc.collection)
    {
        let title_field = target_def.title_field().map(|s| s.to_string());

        let fq = query::FindQuery::builder()
            .filters(vec![query::FilterClause::Single(query::Filter {
                field: jc.on.clone(),
                op: query::FilterOp::Equals(doc_id_str.to_string()),
            })])
            .build();

        if let Ok(docs) = query::find(conn, &jc.collection, target_def, &fq, rel_locale_ctx) {
            let items: Vec<JoinItem> = docs
                .iter()
                .map(|doc| {
                    let label = title_field
                        .as_ref()
                        .and_then(|f| doc.get_str(f))
                        .unwrap_or(&doc.id)
                        .to_string();
                    JoinItem {
                        id: doc.id.to_string(),
                        label,
                    }
                })
                .collect();

            jf.join_count = Some(items.len());
            jf.join_items = Some(items);
        }
    }
}

/// Build a typed attribute object for a richtext node field definition.
fn build_node_attr(f: &FieldDefinition) -> RichtextNodeAttrCtx {
    let label = f
        .admin
        .label
        .as_ref()
        .map(|ls| ls.resolve_default().to_string())
        .unwrap_or_else(|| to_title_case(&f.name));

    let options = if f.options.is_empty() {
        None
    } else {
        Some(
            f.options
                .iter()
                .map(|o| RichtextNodeAttrOption {
                    label: o.label.resolve_default().to_string(),
                    value: o.value.clone(),
                })
                .collect(),
        )
    };

    RichtextNodeAttrCtx {
        name: f.name.clone(),
        kind: f.field_type.as_str().to_string(),
        label,
        required: f.required,
        default: f.default_value.clone(),
        options,
        placeholder: f
            .admin
            .placeholder
            .as_ref()
            .map(|ls| ls.resolve_default().to_string()),
        description: f
            .admin
            .description
            .as_ref()
            .map(|ls| ls.resolve_default().to_string()),
        hidden: if f.admin.hidden { Some(true) } else { None },
        readonly: if f.admin.readonly { Some(true) } else { None },
        width: f.admin.width.clone(),
        step: f.admin.step.clone(),
        rows: f.admin.rows,
        language: f.admin.language.clone(),
        min: f.min,
        max: f.max,
        min_length: f.min_length,
        max_length: f.max_length,
        min_date: f.min_date.clone(),
        max_date: f.max_date.clone(),
        picker_appearance: f.picker_appearance.clone(),
    }
}

/// Enrich a top-level Richtext field context with custom node definitions from registry.
pub(super) fn enrich_richtext(rf: &mut RichtextField, reg: &Registry) {
    let Some(names) = rf.node_names.as_ref() else {
        return;
    };

    let node_defs: Vec<RichtextNodeDefCtx> = names
        .iter()
        .filter_map(|name| reg.get_richtext_node(name))
        .map(|def| RichtextNodeDefCtx {
            name: def.name.clone(),
            label: def.label.clone(),
            inline: def.inline,
            attrs: def.attrs.iter().map(build_node_attr).collect(),
        })
        .collect();

    if !node_defs.is_empty() {
        rf.custom_nodes = Some(node_defs);
    }

    // Drop the now-resolved node-name list from the JSON output.
    rf.node_names = None;
}
