//! DB-access enrichment logic for field contexts.

use std::collections::HashMap;

use serde_json::{Value, json};

use crate::{
    admin::{
        AdminState,
        handlers::field_context::enrich::{
            EnrichCtx, EnrichOptions, enrich_types, nested::enrich_nested_fields,
        },
    },
    core::{
        Registry,
        field::{FieldDefinition, FieldType, RelationshipConfig},
    },
    db::{
        DbConnection,
        query::{self, LocaleContext},
    },
};

/// Parse a "collection/id" composite string into a (collection, id) pair.
fn parse_composite_ref(s: &str) -> Option<(String, String)> {
    let (col, id) = s.split_once('/')?;

    if col.is_empty() || id.is_empty() {
        return None;
    }

    Some((col.to_string(), id.to_string()))
}

/// Extract polymorphic "collection/id" refs from a field value.
fn extract_polymorphic_refs(
    doc_fields: &HashMap<String, Value>,
    field_name: &str,
    has_many: bool,
) -> Vec<(String, String)> {
    if has_many {
        match doc_fields.get(field_name) {
            Some(Value::Array(arr)) => arr
                .iter()
                .filter_map(|v| v.as_str().and_then(parse_composite_ref))
                .collect(),
            _ => Vec::new(),
        }
    } else {
        match doc_fields.get(field_name) {
            Some(Value::String(s)) if !s.is_empty() => parse_composite_ref(s).into_iter().collect(),
            _ => Vec::new(),
        }
    }
}

/// Resolve a single polymorphic ref to a JSON item with id, label, and collection.
fn resolve_polymorphic_ref(
    col: &str,
    id: &str,
    reg: &Registry,
    conn: &dyn DbConnection,
    locale_ctx: Option<&LocaleContext>,
) -> Option<Value> {
    let related_def = reg.get_collection(col)?;
    let title_field = related_def.title_field().map(|s| s.to_string());

    let doc = query::find_by_id(conn, col, related_def, id, locale_ctx)
        .ok()
        .flatten()?;

    let label = title_field
        .as_ref()
        .and_then(|f| doc.get_str(f))
        .unwrap_or(&doc.id)
        .to_string();

    Some(json!({ "id": format!("{}/{}", col, doc.id), "label": label, "collection": col }))
}

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
    let refs = extract_polymorphic_refs(doc_fields, field_name, rc.has_many);

    refs.iter()
        .filter_map(|(col, id)| resolve_polymorphic_ref(col, id, reg, conn, locale_ctx))
        .collect()
}

/// Dispatch enrichment for a single field context based on its type.
fn enrich_single_field(
    ctx: &mut Value,
    field_def: &FieldDefinition,
    doc_fields: &HashMap<String, Value>,
    state: &AdminState,
    opts: &EnrichOptions,
    enrich_ctx: &EnrichCtx,
) {
    let conn = enrich_ctx.conn;
    let reg = enrich_ctx.reg;
    let rel_locale_ctx = enrich_ctx.rel_locale_ctx;

    match field_def.field_type {
        FieldType::Relationship => {
            enrich_types::enrich_relationship(
                ctx,
                field_def,
                doc_fields,
                conn,
                reg,
                rel_locale_ctx,
            );
        }
        FieldType::Upload => {
            enrich_types::enrich_upload(ctx, field_def, doc_fields, conn, reg, rel_locale_ctx);
        }
        FieldType::Array => {
            enrich_types::enrich_array(ctx, field_def, doc_fields, enrich_ctx);
        }
        FieldType::Blocks => {
            enrich_types::enrich_blocks(ctx, field_def, doc_fields, enrich_ctx);
        }
        FieldType::Row | FieldType::Collapsible => {
            if let Some(sub_arr) = ctx.get_mut("sub_fields").and_then(|v| v.as_array_mut()) {
                enrich_field_contexts(sub_arr, &field_def.fields, doc_fields, state, opts);
            }
        }
        FieldType::Group => {
            if let Some(sub_arr) = ctx.get_mut("sub_fields").and_then(|v| v.as_array_mut()) {
                enrich_nested_fields(sub_arr, &field_def.fields, conn, reg, rel_locale_ctx);
            }
        }
        FieldType::Tabs => {
            enrich_tabs(ctx, field_def, doc_fields, state, opts);
        }
        FieldType::Join => {
            enrich_types::enrich_join(ctx, field_def, conn, reg, rel_locale_ctx, opts.doc_id);
        }
        FieldType::Richtext => {
            enrich_types::enrich_richtext(ctx, reg);
        }
        _ => {}
    }
}

/// Recursively enrich sub-fields within each tab.
fn enrich_tabs(
    ctx: &mut Value,
    field_def: &FieldDefinition,
    doc_fields: &HashMap<String, Value>,
    state: &AdminState,
    opts: &EnrichOptions,
) {
    let Some(tabs_arr) = ctx.get_mut("tabs").and_then(|v| v.as_array_mut()) else {
        return;
    };

    for (tab_ctx, tab_def) in tabs_arr.iter_mut().zip(field_def.tabs.iter()) {
        if let Some(sub_arr) = tab_ctx.get_mut("sub_fields").and_then(|v| v.as_array_mut()) {
            enrich_field_contexts(sub_arr, &tab_def.fields, doc_fields, state, opts);
        }
    }
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
    let reg = &state.registry;
    let conn = match state.pool.get() {
        Ok(c) => c,
        Err(_) => return,
    };

    let rel_locale_ctx = LocaleContext::from_locale_string(None, &state.config.locale);

    let enrich_ctx = EnrichCtx {
        state,
        non_default_locale: opts.non_default_locale,
        errors: opts.errors,
        conn: &conn as &dyn DbConnection,
        reg,
        rel_locale_ctx: rel_locale_ctx.as_ref(),
    };

    let defs_iter: Box<dyn Iterator<Item = &FieldDefinition>> = if opts.filter_hidden {
        Box::new(field_defs.iter().filter(|f| !f.admin.hidden))
    } else {
        Box::new(field_defs.iter())
    };

    for (ctx, field_def) in fields.iter_mut().zip(defs_iter) {
        enrich_single_field(ctx, field_def, doc_fields, state, opts, &enrich_ctx);
    }
}
