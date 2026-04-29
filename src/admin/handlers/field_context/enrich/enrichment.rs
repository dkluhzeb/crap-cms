//! DB-access enrichment logic for field contexts.

use std::collections::HashMap;

use serde_json::Value;

use crate::{
    admin::{
        AdminState,
        context::field::{FieldContext, RelationshipSelectedItem, TabsField},
        handlers::field_context::enrich::{
            EnrichCtx, EnrichOptions, enrich_types, nested::enrich_nested_fields,
        },
    },
    core::{
        Registry,
        field::{FieldDefinition, RelationshipConfig},
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

/// Resolve a single polymorphic ref to a typed item with id, label, and collection.
fn resolve_polymorphic_ref(
    col: &str,
    id: &str,
    reg: &Registry,
    conn: &dyn DbConnection,
    locale_ctx: Option<&LocaleContext>,
) -> Option<RelationshipSelectedItem> {
    let related_def = reg.get_collection(col)?;
    let title_field = related_def.title_field().map(|s| s.to_string());

    // Internal UI enrichment — direct query for display labels, not a user-facing read.
    let doc = query::find_by_id(conn, col, related_def, id, locale_ctx)
        .ok()
        .flatten()?;

    let label = title_field
        .as_ref()
        .and_then(|f| doc.get_str(f))
        .unwrap_or(&doc.id)
        .to_string();

    Some(RelationshipSelectedItem {
        id: format!("{}/{}", col, doc.id),
        label,
        collection: Some(col.to_string()),
        ..Default::default()
    })
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
) -> Vec<RelationshipSelectedItem> {
    let refs = extract_polymorphic_refs(doc_fields, field_name, rc.has_many);

    refs.iter()
        .filter_map(|(col, id)| resolve_polymorphic_ref(col, id, reg, conn, locale_ctx))
        .collect()
}

/// Dispatch enrichment for a single typed field context based on its variant.
fn enrich_single_field(
    fc: &mut FieldContext,
    field_def: &FieldDefinition,
    doc_fields: &HashMap<String, Value>,
    state: &AdminState,
    opts: &EnrichOptions,
    enrich_ctx: &EnrichCtx,
) {
    let conn = enrich_ctx.conn;
    let reg = enrich_ctx.reg;
    let rel_locale_ctx = enrich_ctx.rel_locale_ctx;

    match fc {
        FieldContext::Relationship(rf) => {
            enrich_types::enrich_relationship(rf, field_def, doc_fields, conn, reg, rel_locale_ctx);
        }
        FieldContext::Upload(uf) => {
            enrich_types::enrich_upload(uf, field_def, doc_fields, conn, reg, rel_locale_ctx);
        }
        FieldContext::Array(af) => {
            enrich_types::enrich_array(af, field_def, doc_fields, enrich_ctx);
        }
        FieldContext::Blocks(bf) => {
            enrich_types::enrich_blocks(bf, field_def, doc_fields, enrich_ctx);
        }
        FieldContext::Row(rf) => {
            enrich_field_contexts(
                &mut rf.sub_fields,
                &field_def.fields,
                doc_fields,
                state,
                opts,
            );
        }
        FieldContext::Collapsible(cf) => {
            enrich_field_contexts(
                &mut cf.sub_fields,
                &field_def.fields,
                doc_fields,
                state,
                opts,
            );
        }
        FieldContext::Group(gf) => {
            enrich_nested_fields(
                &mut gf.sub_fields,
                &field_def.fields,
                conn,
                reg,
                rel_locale_ctx,
            );
        }
        FieldContext::Tabs(tf) => {
            enrich_tabs(tf, field_def, doc_fields, state, opts);
        }
        FieldContext::Join(jf) => {
            enrich_types::enrich_join(jf, field_def, conn, reg, rel_locale_ctx, opts.doc_id);
        }
        FieldContext::Richtext(rf) => {
            enrich_types::enrich_richtext(rf, reg);
        }
        _ => {}
    }
}

/// Recursively enrich sub-fields within each tab.
fn enrich_tabs(
    tf: &mut TabsField,
    field_def: &FieldDefinition,
    doc_fields: &HashMap<String, Value>,
    state: &AdminState,
    opts: &EnrichOptions,
) {
    for (tab_panel, tab_def) in tf.tabs.iter_mut().zip(field_def.tabs.iter()) {
        enrich_field_contexts(
            &mut tab_panel.sub_fields,
            &tab_def.fields,
            doc_fields,
            state,
            opts,
        );
    }
}

/// Enrich field contexts with data that requires DB access:
/// - Relationship fields: fetch available options from related collection
/// - Array fields: populate existing rows from hydrated document data
/// - Upload fields: fetch upload collection options with thumbnails
/// - Blocks fields: populate block rows from hydrated document data
///
/// Operates on typed [`FieldContext`] end-to-end — no Value roundtrip.
pub fn enrich_field_contexts(
    fields: &mut [FieldContext],
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

    let rel_locale_ctx =
        LocaleContext::from_locale_string(None, &state.config.locale).unwrap_or(None);

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

    for (fc, field_def) in fields.iter_mut().zip(defs_iter) {
        enrich_single_field(fc, field_def, doc_fields, state, opts, &enrich_ctx);
    }
}
