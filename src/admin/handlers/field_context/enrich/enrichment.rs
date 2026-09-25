//! DB-access enrichment logic for field contexts.

use serde_json::Value;

use crate::{
    admin::{
        AdminState,
        context::field::{FieldContext, RelationshipSelectedItem, TabsField},
        handlers::{
            field_context::{
                builder::visible_field_defs,
                cascaded_readonly,
                enrich::{
                    EnrichCtx, EnrichOptions, gated_find_by_id, join::enrich_join,
                    nested::enrich_nested_fields, types,
                },
            },
            shared::admin_form_fields,
        },
    },
    config::LocaleConfig,
    core::{DocumentFields, FieldDefinition, RelationshipConfig},
    db::{DbConnection, LocaleContext, query::poly_ref},
};

/// Extract polymorphic "collection/id" refs from a field value. A has-many
/// value is a JSON array of refs, or — on the validation-error re-render,
/// which feeds form-extracted data — that array as a JSON string.
fn extract_polymorphic_refs(value: Option<&Value>, has_many: bool) -> Vec<(String, String)> {
    let refs: Vec<String> = match value {
        Some(Value::Array(arr)) if has_many => arr
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        Some(Value::String(s)) if has_many => {
            serde_json::from_str::<Vec<String>>(s).unwrap_or_default()
        }
        Some(Value::String(s)) if !s.is_empty() => vec![s.clone()],
        _ => Vec::new(),
    };

    refs.iter().filter_map(|r| poly_ref::parse(r)).collect()
}

/// Resolve a single polymorphic ref to a typed item with id, label, and
/// collection. A ref the viewer cannot resolve (unknown collection, or a
/// target they may not read) keeps its composite id as an unavailable item.
fn resolve_polymorphic_ref(col: &str, id: &str, ctx: &EnrichCtx) -> RelationshipSelectedItem {
    resolve_readable_polymorphic_ref(col, id, ctx).unwrap_or_else(|| RelationshipSelectedItem {
        collection: Some(col.to_string()),
        ..RelationshipSelectedItem::unavailable(poly_ref::format(col, id))
    })
}

/// The labelled item for polymorphic ref `col/id`, when the viewer may read it.
fn resolve_readable_polymorphic_ref(
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
        id: poly_ref::format(col, &doc.id),
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
    polymorphic_selected_from_value(rc, doc_fields.get(field_name), ctx)
}

/// Build `selected_items` for a polymorphic relationship from its raw value —
/// the top-level field's document value or a nested field's row value.
pub(in crate::admin::handlers::field_context) fn polymorphic_selected_from_value(
    rc: &RelationshipConfig,
    value: Option<&Value>,
    ctx: &EnrichCtx,
) -> Vec<RelationshipSelectedItem> {
    let refs = extract_polymorphic_refs(value, rc.has_many);

    refs.iter()
        .map(|(col, id)| resolve_polymorphic_ref(col, id, ctx))
        .collect()
}

/// The enrichment context for the fields inside `field_def`: its own
/// `admin.readonly` joins whatever the container already inherited, so a
/// read-only container locks everything it contains. Everything else carries
/// over — one pooled connection for the whole tree.
fn ctx_inside<'a>(ctx: &EnrichCtx<'a>, field_def: &FieldDefinition) -> EnrichCtx<'a> {
    EnrichCtx {
        ancestor_readonly: cascaded_readonly(field_def, ctx.ancestor_readonly),
        ..*ctx
    }
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
                &ctx_inside(enrich_ctx, field_def),
            );
        }
        FieldContext::Collapsible(cf) => {
            enrich_nested_aligned(
                &mut cf.sub_fields,
                &field_def.fields,
                doc_fields,
                opts,
                &ctx_inside(enrich_ctx, field_def),
            );
        }
        FieldContext::Group(gf) => {
            enrich_nested_fields(
                &mut gf.sub_fields,
                &field_def.fields,
                &ctx_inside(enrich_ctx, field_def),
            );
        }
        FieldContext::Tabs(tf) => {
            enrich_tabs(tf, field_def, doc_fields, opts, enrich_ctx);
        }
        FieldContext::Join(jf) => {
            enrich_join(jf, field_def, enrich_ctx);
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
    let inner = ctx_inside(enrich_ctx, field_def);

    for (tab_panel, tab_def) in tf.tabs.iter_mut().zip(field_def.tabs.iter()) {
        enrich_nested_aligned(
            &mut tab_panel.sub_fields,
            &tab_def.fields,
            doc_fields,
            opts,
            &inner,
        );
    }
}

/// Enrich a nested sub-field context list (layout wrappers: Row/Collapsible/
/// Tabs) against its defs.
///
/// `build_layout_sub_fields` builds one context per def the form renders, so
/// the defs are filtered through the same [`admin_form_fields`] here and the
/// `zip` pairs each context with the def it was built from. Reuses the parent
/// [`EnrichCtx`] so the whole tree shares one pooled connection.
fn enrich_nested_aligned(
    fields: &mut [FieldContext],
    field_defs: &[FieldDefinition],
    doc_fields: &DocumentFields,
    opts: &EnrichOptions,
    enrich_ctx: &EnrichCtx,
) {
    for (fc, field_def) in fields.iter_mut().zip(admin_form_fields(field_defs)) {
        enrich_single_field(fc, field_def, doc_fields, opts, enrich_ctx);
    }
}

/// The locale relationship labels are read in: the editor's, else the default
/// locale — `None` when localization is off.
fn relationship_locale_ctx(opts: &EnrichOptions, config: &LocaleConfig) -> Option<LocaleContext> {
    opts.locale_ctx
        .cloned()
        .or_else(|| LocaleContext::default_for(config))
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
    let reg = &state.infra.registry;
    let Ok(conn) = state.infra.pool.get() else {
        return;
    };

    let rel_locale_ctx = relationship_locale_ctx(opts, &state.config.locale);

    let enrich_ctx = EnrichCtx {
        state,
        non_default_locale: opts.non_default_locale,
        errors: opts.errors,
        conn: &conn as &dyn DbConnection,
        reg,
        rel_locale_ctx: rel_locale_ctx.as_ref(),
        user: opts.user,
        doc_id: opts.doc_id,
        ancestor_readonly: false,
    };

    // Filter the top-level defs through the SAME single source of truth as
    // `build_field_contexts` so this `zip` pairs each enriched context with the
    // def it was built from. Nested sub-fields filter through
    // `admin_form_fields` in `enrich_nested_aligned` / `enrich_nested_fields`,
    // the same filter their builders used.
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
            build_value_contexts, enrich_field_contexts_values, enrich_nested_fields_values,
            make_field, make_test_state, make_test_state_with_registry,
        },
        core::{
            BlockDefinition, CollectionDefinition, DocumentFields, FieldTab, FieldType,
            LocalizedString, Registry, RelationshipConfig, upload::CollectionUpload,
        },
        db::LocaleMode,
    };

    /// Regression: relationship labels were always read in the default locale,
    /// while the list and edit views read in the editor's locale.
    #[test]
    fn relationship_labels_read_in_the_editor_locale() {
        let config = LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: true,
        };
        let de = LocaleContext {
            mode: LocaleMode::Single("de".to_string()),
            config: config.clone(),
        };
        let errors = HashMap::new();

        let editor = EnrichOptions::builder(&errors)
            .locale_ctx(Some(&de))
            .build();
        assert_eq!(
            relationship_locale_ctx(&editor, &config).map(|c| c.access_locale().to_string()),
            Some("de".to_string())
        );

        let none = EnrichOptions::builder(&errors).build();
        assert_eq!(
            relationship_locale_ctx(&none, &config).map(|c| c.access_locale().to_string()),
            Some("en".to_string())
        );
    }

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

    /// Regression: a non-polymorphic relationship field now carries the target
    /// collection's singular label, so the inline-create action can render
    /// `Create new <singular>` instead of a bare `Create new `.
    #[test]
    fn enrich_relationship_sets_target_collection_singular_name() {
        let mut tags = CollectionDefinition::new("tags");
        tags.labels.singular = Some(LocalizedString::Plain("Tag".to_string()));

        let mut registry = Registry::default();
        registry.register_collection(tags);

        let mut rel = FieldDefinition::builder("primary_tag", FieldType::Relationship).build();
        rel.relationship = Some(RelationshipConfig::new("tags", false));
        let field_defs = vec![rel];

        let values = HashMap::new();
        let errors = HashMap::new();
        let mut contexts = build_value_contexts(&field_defs, &values, &errors, false, false);

        let doc_fields = DocumentFields::new();
        let state = make_test_state_with_registry(registry);

        enrich_field_contexts_values(
            &mut contexts,
            &field_defs,
            &doc_fields,
            &state,
            &EnrichOptions::builder(&errors).build(),
        );

        assert_eq!(
            contexts[0]["collection_singular_name"], "Tag",
            "relationship enrichment must expose the target's singular label"
        );
    }

    /// Regression: a stored reference the viewer cannot resolve (unreadable,
    /// trashed, or otherwise unfetchable target) was dropped from
    /// `selected_items`, so the form submitted nothing for it and a save
    /// cleared the reference. Every stored id stays selected — an unresolvable
    /// one as an `unavailable` item carrying no label.
    #[test]
    fn unresolvable_references_stay_selected_as_unavailable() {
        let mut registry = Registry::default();
        registry.register_collection(CollectionDefinition::new("tags"));
        let mut media = CollectionDefinition::new("media");
        media.upload = Some(CollectionUpload::new());
        registry.register_collection(media);

        let reference = |name: &str, ft: FieldType, target: &str, has_many: bool| {
            let mut field = FieldDefinition::builder(name, ft).build();
            field.relationship = Some(RelationshipConfig::new(target, has_many));
            field
        };
        let field_defs = vec![
            reference("tag", FieldType::Relationship, "tags", false),
            reference("tags", FieldType::Relationship, "tags", true),
            reference("image", FieldType::Upload, "media", false),
            reference("images", FieldType::Upload, "media", true),
        ];

        let values = HashMap::from([
            ("tag".to_string(), "t1".to_string()),
            ("image".to_string(), "m1".to_string()),
        ]);
        let errors = HashMap::new();
        let mut contexts = build_value_contexts(&field_defs, &values, &errors, false, false);

        let mut doc_fields = DocumentFields::new();
        doc_fields.insert("tag".to_string(), json!("t1"));
        doc_fields.insert("tags".to_string(), json!(["t1", "t2"]));
        doc_fields.insert("image".to_string(), json!("m1"));
        doc_fields.insert("images".to_string(), json!(["m1"]));

        // The test database has no `tags` / `media` tables: no target resolves.
        let state = make_test_state_with_registry(registry);
        enrich_field_contexts_values(
            &mut contexts,
            &field_defs,
            &doc_fields,
            &state,
            &EnrichOptions::builder(&errors).build(),
        );

        let expected = [
            ("tag", vec!["t1"]),
            ("tags", vec!["t1", "t2"]),
            ("image", vec!["m1"]),
            ("images", vec!["m1"]),
        ];
        for (idx, (name, ids)) in expected.iter().enumerate() {
            let items = contexts[idx]["selected_items"]
                .as_array()
                .unwrap_or_else(|| panic!("{name}: selected_items"));

            let got: Vec<&str> = items.iter().filter_map(|i| i["id"].as_str()).collect();
            assert_eq!(&got, ids, "{name}: every stored id stays selected");

            for item in items {
                assert_eq!(item["unavailable"], true, "{name}: {item}");
                assert_eq!(item["label"], "", "{name}: no label is surfaced");
            }
        }
    }

    /// Regression: a polymorphic relationship inside a row was not resolved —
    /// has-many left with no selection, has-one looked up against the first
    /// target collection — so the form submitted nothing and a save cleared
    /// the stored `collection/id` refs. Each ref now resolves in its own
    /// collection, and one that cannot be resolved stays as an unavailable item.
    #[test]
    fn nested_polymorphic_refs_resolve_per_collection_and_are_kept() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE users (
                id TEXT PRIMARY KEY, name TEXT,
                _status TEXT DEFAULT 'published', created_at TEXT, updated_at TEXT
            );
            INSERT INTO users (id, name) VALUES ('u1', 'Alice');",
        )
        .unwrap();

        let mut users = CollectionDefinition::new("users");
        users.fields = vec![make_field("name", FieldType::Text)];
        users.admin.use_as_title = Some("name".to_string());

        let mut registry = Registry::new();
        registry.register_collection(CollectionDefinition::new("ghosts"));
        registry.register_collection(users);

        let poly = |name: &str, has_many: bool| {
            let mut rc = RelationshipConfig::new("ghosts", has_many);
            rc.polymorphic = vec!["ghosts".into(), "users".into()];
            let mut field = make_field(name, FieldType::Relationship);
            field.relationship = Some(rc);
            field
        };
        let field_defs = vec![poly("one", false), poly("many", true)];

        let mut sub_fields = vec![
            json!({
                "name": "items[0][one]",
                "field_type": "relationship",
                "value": "users/u1",
                "relationship_collection": "ghosts",
            }),
            json!({
                "name": "items[0][many]",
                "field_type": "relationship",
                "value": ["users/u1", "ghosts/g1"],
                "relationship_collection": "ghosts",
                "has_many": true,
            }),
        ];

        enrich_nested_fields_values(&mut sub_fields, &field_defs, &conn, &registry, None);

        let one = sub_fields[0]["selected_items"]
            .as_array()
            .expect("has-one items");
        assert_eq!(one.len(), 1);
        assert_eq!(one[0]["id"], "users/u1");
        assert_eq!(one[0]["label"], "Alice");

        let many = sub_fields[1]["selected_items"]
            .as_array()
            .expect("has-many items");
        assert_eq!(many.len(), 2);
        assert_eq!(many[0]["id"], "users/u1");
        assert_eq!(many[0]["label"], "Alice");
        assert_eq!(many[1]["id"], "ghosts/g1");
        assert_eq!(many[1]["unavailable"], true);
        assert_eq!(many[1]["collection"], "ghosts");
    }

    /// The array edit form round-trips each row's stored junction id: enrichment
    /// exposes `row_id` (the value) and `id_input_name` (the hidden input's
    /// `name`), so the diff-based writer can match the row and preserve a
    /// write-denied sub-field on save.
    #[test]
    fn enrich_array_row_carries_stored_id_for_round_trip() {
        let array_field = FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![make_field("label", FieldType::Text)])
            .build();
        let field_defs = vec![array_field];

        let values = HashMap::new();
        let errors = HashMap::new();
        let mut contexts = build_value_contexts(&field_defs, &values, &errors, false, false);

        let mut doc_fields = DocumentFields::new();
        doc_fields.insert(
            "items".to_string(),
            json!([{ "id": "row-abc", "label": "A" }]),
        );

        let state = make_test_state();
        enrich_field_contexts_values(
            &mut contexts,
            &field_defs,
            &doc_fields,
            &state,
            &EnrichOptions::builder(&errors).build(),
        );

        let rows = contexts[0]["rows"].as_array().expect("array has rows");
        assert_eq!(rows[0]["row_id"], "row-abc", "stored id is exposed");
        assert_eq!(
            rows[0]["id_input_name"], "items[0][id]",
            "hidden id input is named for the form round-trip"
        );
    }
}
