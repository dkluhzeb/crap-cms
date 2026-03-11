//! DB-access enrichment for field contexts (relationship options, array rows, upload thumbnails).

mod field_types;

use crate::admin::handlers::field_context::builder::apply_field_type_extras;
use crate::admin::handlers::field_context::{MAX_FIELD_DEPTH, count_errors_in_fields};
use crate::admin::handlers::shared::auto_label_from_name;
use crate::core::field::FieldType;
use std::collections::HashMap;

/// Build a sub-field context for a single field within an array/blocks row,
/// recursively handling nested composite sub-fields.
///
/// Build enriched child field contexts from structured JSON data.
/// Used by layout wrapper handlers (Tabs/Row/Collapsible) inside Array/Blocks
/// rows to correctly propagate structured data to nested layout wrappers.
///
/// For each child field:
/// - Layout wrappers get transparent names and the whole parent data object
/// - Leaf fields get `parent_name[field_name]` names and their specific value
/// - Recursion handles arbitrary nesting depth (Row inside Tabs inside Array, etc.)
pub fn build_enriched_children_from_data(
    fields: &[crate::core::field::FieldDefinition],
    data: Option<&serde_json::Value>,
    parent_name: &str,
    locale_locked: bool,
    non_default_locale: bool,
    depth: usize,
    errors: &HashMap<String, String>,
) -> Vec<serde_json::Value> {
    if depth >= MAX_FIELD_DEPTH {
        return Vec::new();
    }

    let data_obj = data.and_then(|v| v.as_object());

    fields
        .iter()
        .map(|child| {
            let is_wrapper = matches!(
                child.field_type,
                FieldType::Tabs | FieldType::Row | FieldType::Collapsible
            );

            let child_raw = if is_wrapper {
                data // pass whole object
            } else {
                data_obj.and_then(|m| m.get(&child.name))
            };

            let child_name = if is_wrapper {
                parent_name.to_string() // transparent
            } else {
                format!("{}[{}]", parent_name, child.name)
            };

            let child_val = child_raw
                .map(|v| match v {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Null => String::new(),
                    other => {
                        if is_wrapper {
                            String::new()
                        } else {
                            other.to_string()
                        }
                    }
                })
                .unwrap_or_default();

            let child_label = child
                .admin
                .label
                .as_ref()
                .map(|ls| ls.resolve_default().to_string())
                .unwrap_or_else(|| auto_label_from_name(&child.name));

            let mut child_ctx = serde_json::json!({
                "name": child_name,
                "field_type": child.field_type.as_str(),
                "label": child_label,
                "value": child_val,
                "required": child.required,
                "readonly": child.admin.readonly || locale_locked,
                "locale_locked": locale_locked,
                "placeholder": child.admin.placeholder.as_ref().map(|ls| ls.resolve_default()),
                "description": child.admin.description.as_ref().map(|ls| ls.resolve_default()),
            });

            if let Some(err) = errors.get(&child_name) {
                child_ctx["error"] = serde_json::json!(err);
            }

            match child.field_type {
                FieldType::Row | FieldType::Collapsible => {
                    let sub_fields = build_enriched_children_from_data(
                        &child.fields,
                        child_raw,
                        &child_name,
                        locale_locked,
                        non_default_locale,
                        depth + 1,
                        errors,
                    );
                    child_ctx["sub_fields"] = serde_json::json!(sub_fields);
                    if child.field_type == FieldType::Collapsible {
                        child_ctx["collapsed"] = serde_json::json!(child.admin.collapsed);
                    }
                }
                FieldType::Tabs => {
                    let tabs_ctx: Vec<_> = child
                        .tabs
                        .iter()
                        .map(|tab| {
                            let tab_sub_fields = build_enriched_children_from_data(
                                &tab.fields,
                                child_raw,
                                &child_name,
                                locale_locked,
                                non_default_locale,
                                depth + 1,
                                errors,
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
                        })
                        .collect();
                    child_ctx["tabs"] = serde_json::json!(tabs_ctx);
                }
                _ => {
                    apply_field_type_extras(
                        child,
                        &child_val,
                        &mut child_ctx,
                        &HashMap::new(),
                        errors,
                        &child_name,
                        non_default_locale,
                        depth + 1,
                    );
                }
            }

            child_ctx
        })
        .collect()
}

/// Build an enriched sub-field context for a single field within an array/blocks row.
/// `sf`: the sub-field definition
/// `raw_value`: the raw JSON value for this sub-field from the hydrated document
/// `parent_name`: the parent field's name (e.g. "content")
/// `idx`: the row index within the parent
/// `locale_locked`: whether the parent is locale-locked
/// `non_default_locale`: whether we're on a non-default locale
/// `depth`: nesting depth
pub fn build_enriched_sub_field_context(
    sf: &crate::core::field::FieldDefinition,
    raw_value: Option<&serde_json::Value>,
    parent_name: &str,
    idx: usize,
    locale_locked: bool,
    non_default_locale: bool,
    depth: usize,
    errors: &HashMap<String, String>,
) -> serde_json::Value {
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
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Null => String::new(),
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

    if let Some(err) = errors.get(&indexed_name) {
        sub_ctx["error"] = serde_json::json!(err);
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
        FieldType::Array => field_types::sub_array(
            &mut sub_ctx,
            sf,
            raw_value,
            &indexed_name,
            locale_locked,
            non_default_locale,
            depth,
            errors,
        ),
        FieldType::Blocks => field_types::sub_blocks(
            &mut sub_ctx,
            sf,
            raw_value,
            &indexed_name,
            locale_locked,
            non_default_locale,
            depth,
            errors,
        ),
        FieldType::Group => field_types::sub_group(
            &mut sub_ctx,
            sf,
            raw_value,
            &indexed_name,
            locale_locked,
            non_default_locale,
            depth,
        ),
        FieldType::Row | FieldType::Collapsible => field_types::sub_row_collapsible(
            &mut sub_ctx,
            sf,
            raw_value,
            &indexed_name,
            locale_locked,
            non_default_locale,
            depth,
            errors,
        ),
        FieldType::Tabs => field_types::sub_tabs(
            &mut sub_ctx,
            sf,
            raw_value,
            &indexed_name,
            locale_locked,
            non_default_locale,
            depth,
            errors,
        ),
        FieldType::Text | FieldType::Number if sf.has_many => {
            field_types::sub_has_many_tags(&mut sub_ctx, &val)
        }
        _ => {}
    }

    sub_ctx
}

/// Build selected_items for a polymorphic relationship field.
///
/// Polymorphic values are stored as "collection/id" composites. Each item is
/// looked up in its respective collection to get its label.
pub fn enrich_polymorphic_selected(
    rc: &crate::core::field::RelationshipConfig,
    field_name: &str,
    doc_fields: &HashMap<String, serde_json::Value>,
    reg: &crate::core::Registry,
    conn: &rusqlite::Connection,
    locale_ctx: Option<&crate::db::query::LocaleContext>,
) -> Vec<serde_json::Value> {
    // Parse "collection/id" refs
    let refs: Vec<(String, String)> = if rc.has_many {
        match doc_fields.get(field_name) {
            Some(serde_json::Value::Array(arr)) => arr
                .iter()
                .filter_map(|v| {
                    v.as_str().and_then(|s| {
                        let pos = s.find('/')?;
                        let col = &s[..pos];
                        let id = &s[pos + 1..];
                        if col.is_empty() || id.is_empty() {
                            return None;
                        }
                        Some((col.to_string(), id.to_string()))
                    })
                })
                .collect(),
            _ => Vec::new(),
        }
    } else {
        match doc_fields.get(field_name) {
            Some(serde_json::Value::String(s)) if !s.is_empty() => {
                if let Some(pos) = s.find('/') {
                    let col = &s[..pos];
                    let id = &s[pos + 1..];
                    if !col.is_empty() && !id.is_empty() {
                        vec![(col.to_string(), id.to_string())]
                    } else {
                        Vec::new()
                    }
                } else {
                    Vec::new()
                }
            }
            _ => Vec::new(),
        }
    };

    refs.iter().filter_map(|(col, id)| {
        let related_def = reg.get_collection(col)?;
        let title_field = related_def.title_field().map(|s| s.to_string());
        crate::db::query::find_by_id(conn, col, related_def, id, locale_ctx)
            .ok()
            .flatten()
            .map(|doc| {
                let label = title_field.as_ref()
                    .and_then(|f| doc.get_str(f))
                    .unwrap_or(&doc.id)
                    .to_string();
                // Include collection in the id so JS knows which collection this item belongs to
                serde_json::json!({ "id": format!("{}/{}", col, doc.id), "label": label, "collection": col })
            })
    }).collect()
}

/// Enrich field contexts with data that requires DB access:
/// - Relationship fields: fetch available options from related collection
/// - Array fields: populate existing rows from hydrated document data
/// - Upload fields: fetch upload collection options with thumbnails
/// - Blocks fields: populate block rows from hydrated document data
pub fn enrich_field_contexts(
    fields: &mut [serde_json::Value],
    field_defs: &[crate::core::field::FieldDefinition],
    doc_fields: &HashMap<String, serde_json::Value>,
    state: &crate::admin::AdminState,
    filter_hidden: bool,
    non_default_locale: bool,
    errors: &HashMap<String, String>,
    doc_id: Option<&str>,
) {
    use crate::db::query::LocaleContext;

    let reg = &state.registry;
    let conn = match state.pool.get() {
        Ok(c) => c,
        Err(_) => return,
    };

    let rel_locale_ctx = LocaleContext::from_locale_string(None, &state.config.locale);

    let defs_iter: Box<dyn Iterator<Item = &crate::core::field::FieldDefinition>> = if filter_hidden
    {
        Box::new(field_defs.iter().filter(|f| !f.admin.hidden))
    } else {
        Box::new(field_defs.iter())
    };

    for (ctx, field_def) in fields.iter_mut().zip(defs_iter) {
        match field_def.field_type {
            FieldType::Relationship => {
                field_types::enrich_relationship(
                    ctx,
                    field_def,
                    doc_fields,
                    &conn,
                    reg,
                    rel_locale_ctx.as_ref(),
                );
            }
            FieldType::Array => {
                field_types::enrich_array(
                    ctx,
                    field_def,
                    doc_fields,
                    state,
                    non_default_locale,
                    errors,
                    &conn,
                    reg,
                    rel_locale_ctx.as_ref(),
                );
            }
            FieldType::Upload => {
                field_types::enrich_upload(
                    ctx,
                    field_def,
                    doc_fields,
                    &conn,
                    reg,
                    rel_locale_ctx.as_ref(),
                );
            }
            FieldType::Blocks => {
                field_types::enrich_blocks(
                    ctx,
                    field_def,
                    doc_fields,
                    state,
                    non_default_locale,
                    errors,
                    &conn,
                    reg,
                    rel_locale_ctx.as_ref(),
                );
            }
            FieldType::Row | FieldType::Collapsible => {
                if let Some(sub_arr) = ctx.get_mut("sub_fields").and_then(|v| v.as_array_mut()) {
                    enrich_field_contexts(
                        sub_arr,
                        &field_def.fields,
                        doc_fields,
                        state,
                        filter_hidden,
                        non_default_locale,
                        errors,
                        doc_id,
                    );
                }
            }
            FieldType::Group => {
                if let Some(sub_arr) = ctx.get_mut("sub_fields").and_then(|v| v.as_array_mut()) {
                    enrich_nested_fields(
                        sub_arr,
                        &field_def.fields,
                        &conn,
                        reg,
                        rel_locale_ctx.as_ref(),
                    );
                }
            }
            FieldType::Tabs => {
                if let Some(tabs_arr) = ctx.get_mut("tabs").and_then(|v| v.as_array_mut()) {
                    for (tab_ctx, tab_def) in tabs_arr.iter_mut().zip(field_def.tabs.iter()) {
                        if let Some(sub_arr) =
                            tab_ctx.get_mut("sub_fields").and_then(|v| v.as_array_mut())
                        {
                            enrich_field_contexts(
                                sub_arr,
                                &tab_def.fields,
                                doc_fields,
                                state,
                                filter_hidden,
                                non_default_locale,
                                errors,
                                doc_id,
                            );
                        }
                    }
                }
            }
            FieldType::Join => {
                field_types::enrich_join(
                    ctx,
                    field_def,
                    &conn,
                    reg,
                    rel_locale_ctx.as_ref(),
                    doc_id,
                );
            }
            FieldType::Richtext => {
                field_types::enrich_richtext(ctx, reg);
            }
            _ => {}
        }
    }
}

/// Recursively enrich Upload and Relationship sub-field contexts with options from the database.
/// Called for sub-fields inside layout containers (Row, Collapsible, Tabs, Group) and
/// composite fields (Array, Blocks) that can't be enriched during initial context building.
pub fn enrich_nested_fields(
    sub_fields: &mut [serde_json::Value],
    field_defs: &[crate::core::field::FieldDefinition],
    conn: &rusqlite::Connection,
    reg: &crate::core::Registry,
    rel_locale_ctx: Option<&crate::db::query::LocaleContext>,
) {
    use crate::core::upload;
    use crate::db::query;

    for (ctx, field_def) in sub_fields.iter_mut().zip(field_defs.iter()) {
        match field_def.field_type {
            FieldType::Relationship => {
                if let Some(ref rc) = field_def.relationship {
                    if let Some(related_def) = reg.get_collection(&rc.collection) {
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
                                    ctx["selected_items"] =
                                        serde_json::json!([{ "id": doc.id, "label": label }]);
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
            FieldType::Upload => {
                if let Some(ref rc) = field_def.relationship {
                    if let Some(related_def) = reg.get_collection(&rc.collection) {
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
                                    if let Some(ref uc) = related_def.upload {
                                        if uc.enabled {
                                            upload::assemble_sizes_object(&mut doc, uc);
                                        }
                                    }
                                    let label = doc
                                        .get_str("filename")
                                        .or_else(|| {
                                            title_field.as_ref().and_then(|f| doc.get_str(f))
                                        })
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
                                    let mut item =
                                        serde_json::json!({ "id": doc.id, "label": label });
                                    if let Some(ref url) = thumb_url {
                                        item["thumbnail_url"] = serde_json::json!(url);
                                    }
                                    if is_image {
                                        item["is_image"] = serde_json::json!(true);
                                    }
                                    item["filename"] = serde_json::json!(label);
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
                // Recurse into array rows' sub-fields
                if let Some(rows_arr) = ctx.get_mut("rows").and_then(|v| v.as_array_mut()) {
                    for row_ctx in rows_arr.iter_mut() {
                        if let Some(sub_arr) =
                            row_ctx.get_mut("sub_fields").and_then(|v| v.as_array_mut())
                        {
                            enrich_nested_fields(
                                sub_arr,
                                &field_def.fields,
                                conn,
                                reg,
                                rel_locale_ctx,
                            );
                        }
                    }
                }
                // Enrich the <template> sub-fields so new rows added via JS have upload/relationship options
                if let Some(sub_arr) = ctx.get_mut("sub_fields").and_then(|v| v.as_array_mut()) {
                    enrich_nested_fields(sub_arr, &field_def.fields, conn, reg, rel_locale_ctx);
                }
            }
            FieldType::Blocks => {
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
                        {
                            if let Some(sub_arr) =
                                row_ctx.get_mut("sub_fields").and_then(|v| v.as_array_mut())
                            {
                                enrich_nested_fields(
                                    sub_arr,
                                    &block_def.fields,
                                    conn,
                                    reg,
                                    rel_locale_ctx,
                                );
                            }
                        }
                    }
                }
                // Enrich block definition templates so new block rows have upload/relationship options
                if let Some(defs_arr) = ctx
                    .get_mut("block_definitions")
                    .and_then(|v| v.as_array_mut())
                {
                    for (def_ctx, block_def) in defs_arr.iter_mut().zip(field_def.blocks.iter()) {
                        if let Some(sub_arr) =
                            def_ctx.get_mut("fields").and_then(|v| v.as_array_mut())
                        {
                            enrich_nested_fields(
                                sub_arr,
                                &block_def.fields,
                                conn,
                                reg,
                                rel_locale_ctx,
                            );
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests;
