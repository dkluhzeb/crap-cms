//! DB-access enrichment logic for field contexts.

use serde_json::Value;

use crate::{
    admin::{
        AdminState,
        context::field::{FieldContext, RelationshipSelectedItem, TabsField},
        handlers::field_context::{
            builder::visible_field_defs,
            enrich::{
                EnrichCtx, EnrichOptions, gated_find_by_id, nested::enrich_nested_fields, types,
            },
        },
    },
    core::{DocumentFields, FieldDefinition, RelationshipConfig},
    db::{DbConnection, LocaleContext},
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
    doc_fields: &DocumentFields,
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
    ctx: &EnrichCtx,
) -> Option<RelationshipSelectedItem> {
    let related_def = ctx.reg.get_collection(col)?;
    let title_field = related_def
        .title_field()
        .map(std::string::ToString::to_string);

    // Access-gated: never label a polymorphic target the viewer cannot read.
    let doc = gated_find_by_id(ctx, col, related_def, id)?;

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

/// Build `selected_items` for a polymorphic relationship field.
///
/// Polymorphic values are stored as "collection/id" composites. Each item is
/// looked up in its respective collection to get its label.
pub fn enrich_polymorphic_selected(
    rc: &RelationshipConfig,
    field_name: &str,
    doc_fields: &DocumentFields,
    ctx: &EnrichCtx,
) -> Vec<RelationshipSelectedItem> {
    let refs = extract_polymorphic_refs(doc_fields, field_name, rc.has_many);

    refs.iter()
        .filter_map(|(col, id)| resolve_polymorphic_ref(col, id, ctx))
        .collect()
}

/// Dispatch enrichment for a single typed field context based on its variant.
fn enrich_single_field(
    fc: &mut FieldContext,
    field_def: &FieldDefinition,
    doc_fields: &DocumentFields,
    opts: &EnrichOptions,
    enrich_ctx: &EnrichCtx,
) {
    let reg = enrich_ctx.reg;

    match fc {
        FieldContext::Relationship(rf) => {
            types::enrich_relationship(rf, field_def, doc_fields, enrich_ctx);
        }
        FieldContext::Upload(uf) => {
            types::enrich_upload(uf, field_def, doc_fields, enrich_ctx);
        }
        FieldContext::Array(af) => {
            types::enrich_array(af, field_def, doc_fields, enrich_ctx);
        }
        FieldContext::Blocks(bf) => {
            types::enrich_blocks(bf, field_def, doc_fields, enrich_ctx);
        }
        FieldContext::Row(rf) => {
            enrich_nested_aligned(
                &mut rf.sub_fields,
                &field_def.fields,
                doc_fields,
                opts,
                enrich_ctx,
            );
        }
        FieldContext::Collapsible(cf) => {
            enrich_nested_aligned(
                &mut cf.sub_fields,
                &field_def.fields,
                doc_fields,
                opts,
                enrich_ctx,
            );
        }
        FieldContext::Group(gf) => {
            enrich_nested_fields(&mut gf.sub_fields, &field_def.fields, enrich_ctx);
        }
        FieldContext::Tabs(tf) => {
            enrich_tabs(tf, field_def, doc_fields, opts, enrich_ctx);
        }
        FieldContext::Join(jf) => {
            types::enrich_join(jf, field_def, enrich_ctx, opts.doc_id);
        }
        FieldContext::Richtext(rf) => {
            types::enrich_richtext(rf, reg);
        }
        _ => {}
    }
}

/// Recursively enrich sub-fields within each tab.
fn enrich_tabs(
    tf: &mut TabsField,
    field_def: &FieldDefinition,
    doc_fields: &DocumentFields,
    opts: &EnrichOptions,
    enrich_ctx: &EnrichCtx,
) {
    for (tab_panel, tab_def) in tf.tabs.iter_mut().zip(field_def.tabs.iter()) {
        enrich_nested_aligned(
            &mut tab_panel.sub_fields,
            &tab_def.fields,
            doc_fields,
            opts,
            enrich_ctx,
        );
    }
}

/// Enrich a 1:1-aligned list of nested sub-field contexts against their defs.
///
/// Used by layout wrappers (Row/Collapsible/Tabs) whose sub-field *contexts* are
/// built UNFILTERED by `build_layout_sub_fields` — exactly one context per def,
/// in def order. So the defs are deliberately NOT run through `visible_field_defs`
/// here: filtering them would drop entries the context list still has, desyncing
/// the `zip`. Hidden-field filtering happens once, at the top level (see
/// [`enrich_field_contexts`]), consistently with `build_field_contexts`. Reuses
/// the parent [`EnrichCtx`] so the whole tree shares one pooled connection.
fn enrich_nested_aligned(
    fields: &mut [FieldContext],
    field_defs: &[FieldDefinition],
    doc_fields: &DocumentFields,
    opts: &EnrichOptions,
    enrich_ctx: &EnrichCtx,
) {
    for (fc, field_def) in fields.iter_mut().zip(field_defs.iter()) {
        enrich_single_field(fc, field_def, doc_fields, opts, enrich_ctx);
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
    doc_fields: &DocumentFields,
    state: &AdminState,
    opts: &EnrichOptions,
) {
    let reg = &state.registry;
    let Ok(conn) = state.pool.get() else {
        return;
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
        user: opts.user,
    };

    // Filter the top-level defs through the SAME single source of truth as
    // `build_field_contexts` so this `zip` pairs each enriched context with the
    // def it was built from. Nested layout sub-fields are handled separately by
    // `enrich_nested_aligned` (unfiltered, 1:1) — see its doc comment.
    for (fc, field_def) in fields
        .iter_mut()
        .zip(visible_field_defs(field_defs, opts.filter_hidden))
    {
        enrich_single_field(fc, field_def, doc_fields, opts, &enrich_ctx);
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;

    use super::*;
    use crate::{
        admin::handlers::field_context::enrich::test_helpers::{
            build_value_contexts, enrich_field_contexts_values, make_field, make_test_state,
        },
        core::{BlockDefinition, DocumentFields, FieldTab, FieldType, LocalizedString},
    };

    /// Regression: blocks inside Tabs were not populated from `doc_fields`
    /// because [`enrich_field_contexts`] delegated to `enrich_nested_fields`
    /// instead of recursing into the layout wrapper.
    #[test]
    fn enrich_field_contexts_blocks_inside_tabs_populates_rows() {
        let mut blocks_field = FieldDefinition::builder("content", FieldType::Blocks).build();
        blocks_field.blocks = vec![{
            let mut bd = BlockDefinition::new("hero", vec![make_field("heading", FieldType::Text)]);
            bd.label = Some(LocalizedString::Plain("Hero".to_string()));
            bd
        }];
        let mut tabs_field = FieldDefinition::builder("page_settings", FieldType::Tabs).build();
        tabs_field.tabs = vec![FieldTab::new("Content", vec![blocks_field.clone()])];
        let field_defs = vec![tabs_field];

        let values = HashMap::new();
        let errors = HashMap::new();
        let mut contexts = build_value_contexts(&field_defs, &values, &errors, false, false);

        let mut doc_fields = DocumentFields::new();
        doc_fields.insert(
            "content".to_string(),
            json!([
                {"_block_type": "hero", "heading": "Welcome"},
            ]),
        );

        let state = make_test_state();

        enrich_field_contexts_values(
            &mut contexts,
            &field_defs,
            &doc_fields,
            &state,
            &EnrichOptions::builder(&errors).build(),
        );

        let tabs = contexts[0]["tabs"].as_array().unwrap();
        let tab_sub_fields = tabs[0]["sub_fields"].as_array().unwrap();
        let blocks_ctx = &tab_sub_fields[0];
        assert_eq!(blocks_ctx["field_type"], "blocks");
        let rows = blocks_ctx["rows"]
            .as_array()
            .expect("blocks inside Tabs must have rows populated from doc_fields");
        assert_eq!(rows.len(), 1, "should have 1 block row");
        assert_eq!(rows[0]["_block_type"], "hero");
    }

    /// Regression: arrays inside Row were not populated from `doc_fields`.
    #[test]
    fn enrich_field_contexts_array_inside_row_populates_rows() {
        let array_field = FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![make_field("label", FieldType::Text)])
            .build();
        let row_field = FieldDefinition::builder("main_row", FieldType::Row)
            .fields(vec![array_field])
            .build();
        let field_defs = vec![row_field];

        let values = HashMap::new();
        let errors = HashMap::new();
        let mut contexts = build_value_contexts(&field_defs, &values, &errors, false, false);

        let mut doc_fields = DocumentFields::new();
        doc_fields.insert(
            "items".to_string(),
            json!([
                {"label": "First"},
                {"label": "Second"},
            ]),
        );

        let state = make_test_state();

        enrich_field_contexts_values(
            &mut contexts,
            &field_defs,
            &doc_fields,
            &state,
            &EnrichOptions::builder(&errors).build(),
        );

        let row_sub_fields = contexts[0]["sub_fields"].as_array().unwrap();
        let array_ctx = &row_sub_fields[0];
        assert_eq!(array_ctx["field_type"], "array");
        let rows = array_ctx["rows"]
            .as_array()
            .expect("array inside Row must have rows populated from doc_fields");
        assert_eq!(rows.len(), 2, "should have 2 array rows");
    }

    /// Regression: a top-level `field.hidden` field placed BEFORE other fields
    /// used to desync enrichment. `build_field_contexts` always drops
    /// `field.hidden` (so it produces N-1 contexts), but `enrich_field_contexts`
    /// iterated the raw, unfiltered def list (N defs) — so the `zip` paired the
    /// surviving contexts with the WRONG defs from the hidden field onward, and
    /// the array's rows were looked up under the hidden field's name (→ empty).
    /// Both passes now filter through `visible_field_defs`, so the zip stays
    /// aligned and the array enriches correctly.
    #[test]
    fn enrich_field_contexts_hidden_field_before_array_stays_aligned() {
        let secret = FieldDefinition::builder("secret", FieldType::Text)
            .hidden(true)
            .build();
        let array_field = FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![make_field("label", FieldType::Text)])
            .build();
        let field_defs = vec![secret, array_field];

        let values = HashMap::new();
        let errors = HashMap::new();
        let mut contexts = build_value_contexts(&field_defs, &values, &errors, false, false);

        // `build_field_contexts` always strips `field.hidden`, so only the array
        // context survives.
        assert_eq!(contexts.len(), 1, "hidden field must not produce a context");
        assert_eq!(contexts[0]["field_type"], "array");

        let mut doc_fields = DocumentFields::new();
        doc_fields.insert(
            "items".to_string(),
            json!([
                {"label": "First"},
                {"label": "Second"},
            ]),
        );

        let state = make_test_state();

        enrich_field_contexts_values(
            &mut contexts,
            &field_defs,
            &doc_fields,
            &state,
            &EnrichOptions::builder(&errors).build(),
        );

        let rows = contexts[0]["rows"]
            .as_array()
            .expect("array must enrich despite the preceding hidden field");
        assert_eq!(
            rows.len(),
            2,
            "array rows must populate (enrich stayed aligned)"
        );
    }

    // ── Layout wrappers inside Array: transparent names + data flow ─

    #[test]
    fn enriched_sub_field_tabs_in_array_transparent_names() {
        let mut arr_field = make_field("items", FieldType::Array);
        arr_field.fields = vec![
            FieldDefinition::builder("layout", FieldType::Tabs)
                .tabs(vec![
                    FieldTab::new("General", vec![make_field("title", FieldType::Text)]),
                    FieldTab::new("Content", vec![make_field("body", FieldType::Textarea)]),
                ])
                .build(),
        ];

        let row_data = json!([{"id": "r1", "title": "Hello", "body": "World"}]);

        let fields = vec![arr_field.clone()];
        let values = HashMap::new();
        let errors = HashMap::new();
        let mut contexts = build_value_contexts(&fields, &values, &errors, false, false);

        let mut doc_fields = DocumentFields::new();
        doc_fields.insert("items".to_string(), row_data);

        let state = make_test_state();

        enrich_field_contexts_values(
            &mut contexts,
            &fields,
            &doc_fields,
            &state,
            &EnrichOptions::builder(&errors).build(),
        );

        let rows = contexts[0]["rows"].as_array().expect("should have rows");
        assert_eq!(rows.len(), 1);

        let row_sub_fields = rows[0]["sub_fields"].as_array().unwrap();
        assert_eq!(row_sub_fields.len(), 1);
        assert_eq!(row_sub_fields[0]["field_type"], "tabs");
        // Transparent name: items[0] (not items[0][layout]).
        assert_eq!(row_sub_fields[0]["name"], "items[0]");

        let tabs = row_sub_fields[0]["tabs"].as_array().unwrap();
        assert_eq!(tabs.len(), 2);

        let tab1_fields = tabs[0]["sub_fields"].as_array().unwrap();
        assert_eq!(tab1_fields[0]["name"], "items[0][title]");
        assert_eq!(tab1_fields[0]["value"], "Hello");

        let tab2_fields = tabs[1]["sub_fields"].as_array().unwrap();
        assert_eq!(tab2_fields[0]["name"], "items[0][body]");
        assert_eq!(tab2_fields[0]["value"], "World");
    }

    #[test]
    fn enriched_sub_field_row_in_array_transparent_names() {
        let mut arr_field = make_field("items", FieldType::Array);
        arr_field.fields = vec![
            FieldDefinition::builder("row_wrap", FieldType::Row)
                .fields(vec![
                    make_field("x", FieldType::Text),
                    make_field("y", FieldType::Text),
                ])
                .build(),
        ];

        let row_data = json!([{"id": "r1", "x": "10", "y": "20"}]);

        let fields = vec![arr_field.clone()];
        let values = HashMap::new();
        let errors = HashMap::new();
        let mut contexts = build_value_contexts(&fields, &values, &errors, false, false);

        let mut doc_fields = DocumentFields::new();
        doc_fields.insert("items".to_string(), row_data);

        let state = make_test_state();

        enrich_field_contexts_values(
            &mut contexts,
            &fields,
            &doc_fields,
            &state,
            &EnrichOptions::builder(&errors).build(),
        );

        let rows = contexts[0]["rows"].as_array().expect("should have rows");
        assert_eq!(rows.len(), 1);

        let row_sub_fields = rows[0]["sub_fields"].as_array().unwrap();
        assert_eq!(row_sub_fields.len(), 1);
        assert_eq!(row_sub_fields[0]["field_type"], "row");
        // Transparent name: items[0] (not items[0][row_wrap]).
        assert_eq!(row_sub_fields[0]["name"], "items[0]");

        let children = row_sub_fields[0]["sub_fields"].as_array().unwrap();
        assert_eq!(children[0]["name"], "items[0][x]");
        assert_eq!(children[0]["value"], "10");
        assert_eq!(children[1]["name"], "items[0][y]");
        assert_eq!(children[1]["value"], "20");
    }

    #[test]
    fn enriched_sub_field_row_inside_tabs_in_array_transparent_names() {
        let mut arr_field = make_field("team_members", FieldType::Array);
        arr_field.fields = vec![
            FieldDefinition::builder("member_tabs", FieldType::Tabs)
                .tabs(vec![
                    FieldTab::new(
                        "Personal",
                        vec![
                            FieldDefinition::builder("name_row", FieldType::Row)
                                .fields(vec![
                                    make_field("first_name", FieldType::Text),
                                    make_field("last_name", FieldType::Text),
                                ])
                                .build(),
                            make_field("email", FieldType::Email),
                        ],
                    ),
                    FieldTab::new(
                        "Professional",
                        vec![make_field("job_title", FieldType::Text)],
                    ),
                ])
                .build(),
        ];

        let row_data = json!([
            {"id": "r1", "first_name": "John", "last_name": "Doe", "email": "john@example.com", "job_title": "Dev"}
        ]);

        let fields = vec![arr_field.clone()];
        let values = HashMap::new();
        let errors = HashMap::new();
        let mut contexts = build_value_contexts(&fields, &values, &errors, false, false);

        let mut doc_fields = DocumentFields::new();
        doc_fields.insert("team_members".to_string(), row_data);

        let state = make_test_state();

        enrich_field_contexts_values(
            &mut contexts,
            &fields,
            &doc_fields,
            &state,
            &EnrichOptions::builder(&errors).build(),
        );

        let rows = contexts[0]["rows"].as_array().expect("should have rows");
        assert_eq!(rows.len(), 1);

        let row_sub_fields = rows[0]["sub_fields"].as_array().unwrap();
        assert_eq!(row_sub_fields.len(), 1);
        assert_eq!(row_sub_fields[0]["field_type"], "tabs");
        assert_eq!(row_sub_fields[0]["name"], "team_members[0]");

        let tabs = row_sub_fields[0]["tabs"].as_array().unwrap();
        assert_eq!(tabs.len(), 2);

        // Personal tab: Row (transparent) + email.
        let personal_fields = tabs[0]["sub_fields"].as_array().unwrap();
        assert_eq!(personal_fields.len(), 2);

        assert_eq!(personal_fields[0]["field_type"], "row");
        assert_eq!(personal_fields[0]["name"], "team_members[0]");

        let row_children = personal_fields[0]["sub_fields"].as_array().unwrap();
        assert_eq!(row_children[0]["name"], "team_members[0][first_name]");
        assert_eq!(row_children[0]["value"], "John");
        assert_eq!(row_children[1]["name"], "team_members[0][last_name]");
        assert_eq!(row_children[1]["value"], "Doe");

        assert_eq!(personal_fields[1]["name"], "team_members[0][email]");
        assert_eq!(personal_fields[1]["value"], "john@example.com");

        let pro_fields = tabs[1]["sub_fields"].as_array().unwrap();
        assert_eq!(pro_fields[0]["name"], "team_members[0][job_title]");
        assert_eq!(pro_fields[0]["value"], "Dev");
    }
}
