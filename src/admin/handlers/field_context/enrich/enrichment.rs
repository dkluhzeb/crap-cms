//! DB-access enrichment logic for field contexts.

use std::collections::HashMap;

use serde_json::{Value, json};

use crate::{
    admin::AdminState,
    core::{
        Registry,
        field::{FieldDefinition, FieldType, RelationshipConfig},
    },
    db::{
        DbConnection,
        query::{self, LocaleContext},
    },
};

use super::{EnrichCtx, EnrichOptions, enrich_types, nested::enrich_nested_fields};

/// Build selected_items for a polymorphic relationship field.
///
/// Polymorphic values are stored as "collection/id" composites. Each item is
/// looked up in its respective collection to get its label.
pub fn enrich_polymorphic_selected(
    rc: &RelationshipConfig,
    field_name: &str,
    doc_fields: &HashMap<String, Value>,
    reg: &Registry,
    conn: &dyn DbConnection,
    locale_ctx: Option<&LocaleContext>,
) -> Vec<Value> {
    // Parse "collection/id" refs
    let refs: Vec<(String, String)> = if rc.has_many {
        match doc_fields.get(field_name) {
            Some(Value::Array(arr)) => arr
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
            Some(Value::String(s)) if !s.is_empty() => {
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

    refs.iter()
        .filter_map(|(col, id)| {
            let related_def = reg.get_collection(col)?;
            let title_field = related_def.title_field().map(|s| s.to_string());

            query::find_by_id(conn, col, related_def, id, locale_ctx)
            .ok()
            .flatten()
            .map(|doc| {
                let label = title_field.as_ref()
                    .and_then(|f| doc.get_str(f))
                    .unwrap_or(&doc.id)
                    .to_string();
                // Include collection in the id so JS knows which collection this item belongs to
                json!({ "id": format!("{}/{}", col, doc.id), "label": label, "collection": col })
            })
        })
        .collect()
}

/// Enrich field contexts with data that requires DB access:
/// - Relationship fields: fetch available options from related collection
/// - Array fields: populate existing rows from hydrated document data
/// - Upload fields: fetch upload collection options with thumbnails
/// - Blocks fields: populate block rows from hydrated document data
pub fn enrich_field_contexts(
    fields: &mut [Value],
    field_defs: &[FieldDefinition],
    doc_fields: &HashMap<String, Value>,
    state: &AdminState,
    opts: &EnrichOptions,
) {
    let filter_hidden = opts.filter_hidden;
    let non_default_locale = opts.non_default_locale;
    let errors = opts.errors;
    let doc_id = opts.doc_id;

    let reg = &state.registry;
    let conn = match state.pool.get() {
        Ok(c) => c,
        Err(_) => return,
    };

    let rel_locale_ctx = LocaleContext::from_locale_string(None, &state.config.locale);

    let enrich_ctx = EnrichCtx {
        state,
        non_default_locale,
        errors,
        conn: &conn as &dyn DbConnection,
        reg,
        rel_locale_ctx: rel_locale_ctx.as_ref(),
    };

    let defs_iter: Box<dyn Iterator<Item = &FieldDefinition>> = if filter_hidden {
        Box::new(field_defs.iter().filter(|f| !f.admin.hidden))
    } else {
        Box::new(field_defs.iter())
    };

    for (ctx, field_def) in fields.iter_mut().zip(defs_iter) {
        match field_def.field_type {
            FieldType::Relationship => {
                enrich_types::enrich_relationship(
                    ctx,
                    field_def,
                    doc_fields,
                    &conn as &dyn DbConnection,
                    reg,
                    rel_locale_ctx.as_ref(),
                );
            }
            FieldType::Array => {
                enrich_types::enrich_array(ctx, field_def, doc_fields, &enrich_ctx);
            }
            FieldType::Upload => {
                enrich_types::enrich_upload(
                    ctx,
                    field_def,
                    doc_fields,
                    &conn as &dyn DbConnection,
                    reg,
                    rel_locale_ctx.as_ref(),
                );
            }
            FieldType::Blocks => {
                enrich_types::enrich_blocks(ctx, field_def, doc_fields, &enrich_ctx);
            }
            FieldType::Row | FieldType::Collapsible => {
                if let Some(sub_arr) = ctx.get_mut("sub_fields").and_then(|v| v.as_array_mut()) {
                    enrich_field_contexts(sub_arr, &field_def.fields, doc_fields, state, opts);
                }
            }
            FieldType::Group => {
                if let Some(sub_arr) = ctx.get_mut("sub_fields").and_then(|v| v.as_array_mut()) {
                    enrich_nested_fields(
                        sub_arr,
                        &field_def.fields,
                        &conn as &dyn DbConnection,
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
                                opts,
                            );
                        }
                    }
                }
            }
            FieldType::Join => {
                enrich_types::enrich_join(
                    ctx,
                    field_def,
                    &conn as &dyn DbConnection,
                    reg,
                    rel_locale_ctx.as_ref(),
                    doc_id,
                );
            }
            FieldType::Richtext => {
                enrich_types::enrich_richtext(ctx, reg);
            }
            _ => {}
        }
    }
}
