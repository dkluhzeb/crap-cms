//! Builds enriched sub-field contexts for array/blocks rows and recursively
//! enriches nested relationship/upload fields with DB-fetched options.

use std::collections::HashMap;

use serde_json::Value;

use super::enrich_types::build_upload_item;
use crate::{
    admin::{
        context::field::{
            ArrayField, BaseFieldData, BlocksField, CheckboxField, ChoiceField, CodeField,
            ConditionData, DateField, FieldContext, GroupField, JoinField, NumberField,
            RelationshipField, RelationshipSelectedItem, RichtextField, RowField, TabsField,
            TextField, TextareaField, UploadField, ValidationAttrs,
        },
        handlers::{
            field_context::{
                MAX_FIELD_DEPTH, collect_node_attr_errors,
                enrich::{SubFieldOpts, field_types},
                safe_template_id,
            },
            shared::auto_label_from_name,
        },
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

/// Build the typed shared base data for a sub-field (before variant
/// construction).
fn build_sub_field_base(
    sf: &FieldDefinition,
    indexed_name: &str,
    val: &str,
    opts: &SubFieldOpts,
) -> BaseFieldData {
    let sf_label = sf
        .admin
        .label
        .as_ref()
        .map(|ls| ls.resolve_default().to_string())
        .unwrap_or_else(|| auto_label_from_name(&sf.name));

    BaseFieldData {
        name: indexed_name.to_string(),
        label: sf_label,
        required: sf.required,
        value: Value::String(val.to_string()),
        placeholder: sf
            .admin
            .placeholder
            .as_ref()
            .map(|ls| ls.resolve_default().to_string()),
        description: sf
            .admin
            .description
            .as_ref()
            .map(|ls| ls.resolve_default().to_string()),
        readonly: sf.admin.readonly || opts.locale_locked,
        localized: sf.localized,
        locale_locked: opts.locale_locked,
        position: sf.admin.position.clone(),
        error: opts.errors.get(indexed_name).cloned(),
        validation: ValidationAttrs::default(),
        condition: ConditionData::default(),
    }
}

/// Construct the [`FieldContext`] variant matching `sf.field_type`, with the
/// base data populated and per-variant defaults filled in. Type-specific
/// dispatch in [`dispatch_sub_field_type`] subsequently mutates the variant
/// to set its real data.
pub(super) fn construct_sub_variant(
    sf: &FieldDefinition,
    base: BaseFieldData,
    indexed_name: &str,
) -> FieldContext {
    match &sf.field_type {
        FieldType::Text => FieldContext::Text(TextField {
            base,
            has_many: None,
            tags: None,
        }),
        FieldType::Email => FieldContext::Email(TextField {
            base,
            has_many: None,
            tags: None,
        }),
        FieldType::Json => FieldContext::Json(TextField {
            base,
            has_many: None,
            tags: None,
        }),
        FieldType::Textarea => FieldContext::Textarea(TextareaField {
            base,
            rows: 8,
            resizable: false,
        }),
        FieldType::Number => FieldContext::Number(NumberField {
            base,
            step: String::new(),
            has_many: None,
            tags: None,
        }),
        FieldType::Code => FieldContext::Code(CodeField {
            base,
            language: String::new(),
            languages: None,
        }),
        FieldType::Richtext => FieldContext::Richtext(RichtextField {
            base,
            resizable: false,
            richtext_format: "html".to_string(),
            features: None,
            node_names: None,
            custom_nodes: None,
        }),
        FieldType::Date => FieldContext::Date(DateField {
            base,
            picker_appearance: "dayOnly".to_string(),
            date_only_value: None,
            datetime_local_value: None,
            min_date: None,
            max_date: None,
            timezone_enabled: None,
            default_timezone: None,
            timezone_options: None,
            timezone_value: None,
        }),
        FieldType::Checkbox => FieldContext::Checkbox(CheckboxField {
            base,
            checked: false,
        }),
        FieldType::Select => FieldContext::Select(ChoiceField {
            base,
            options: Vec::new(),
            has_many: None,
        }),
        FieldType::Radio => FieldContext::Radio(ChoiceField {
            base,
            options: Vec::new(),
            has_many: None,
        }),
        FieldType::Relationship => FieldContext::Relationship(RelationshipField {
            base,
            relationship_collection: None,
            has_many: None,
            polymorphic: None,
            collections: None,
            picker: None,
            selected_items: None,
        }),
        FieldType::Upload => FieldContext::Upload(UploadField {
            base,
            relationship_collection: None,
            has_many: None,
            picker: None,
            selected_items: None,
            selected_filename: None,
            selected_preview_url: None,
        }),
        FieldType::Join => FieldContext::Join(JoinField {
            base,
            join_collection: None,
            join_on: None,
            join_items: None,
            join_count: None,
        }),
        FieldType::Group => FieldContext::Group(GroupField {
            base,
            sub_fields: Vec::new(),
            collapsed: false,
        }),
        FieldType::Row => FieldContext::Row(RowField {
            base,
            sub_fields: Vec::new(),
        }),
        FieldType::Collapsible => FieldContext::Collapsible(GroupField {
            base,
            sub_fields: Vec::new(),
            collapsed: false,
        }),
        FieldType::Tabs => FieldContext::Tabs(TabsField {
            base,
            tabs: Vec::new(),
        }),
        FieldType::Array => FieldContext::Array(ArrayField {
            base,
            sub_fields: Vec::new(),
            rows: None,
            row_count: 0,
            template_id: safe_template_id(indexed_name),
            min_rows: None,
            max_rows: None,
            init_collapsed: false,
            add_label: None,
            label_field: None,
        }),
        FieldType::Blocks => FieldContext::Blocks(BlocksField {
            base,
            block_definitions: Vec::new(),
            rows: None,
            row_count: 0,
            template_id: safe_template_id(indexed_name),
            min_rows: None,
            max_rows: None,
            init_collapsed: false,
            add_label: None,
            picker: None,
            label_field: None,
        }),
    }
}

/// Enrich a Richtext sub-field context with format, features, nodes, and attr errors.
pub(super) fn enrich_sub_richtext(
    rf: &mut RichtextField,
    sf: &FieldDefinition,
    indexed_name: &str,
    errors: &HashMap<String, String>,
) {
    rf.resizable = sf.admin.resizable;

    if !sf.admin.features.is_empty() {
        rf.features = Some(sf.admin.features.clone());
    }

    rf.richtext_format = sf
        .admin
        .richtext_format
        .as_deref()
        .unwrap_or("html")
        .to_string();

    if !sf.admin.nodes.is_empty() {
        rf.node_names = Some(sf.admin.nodes.clone());
    }

    if rf.base.error.is_none()
        && let Some(node_err) = collect_node_attr_errors(errors, indexed_name)
    {
        rf.base.error = Some(node_err);
    }
}

/// Dispatch type-specific enrichment for a typed sub-field context.
fn dispatch_sub_field_type(
    fc: &mut FieldContext,
    sf: &FieldDefinition,
    val: &str,
    raw_value: Option<&Value>,
    indexed_name: &str,
    opts: &SubFieldOpts,
) {
    match fc {
        FieldContext::Checkbox(cf) => field_types::sub_checkbox(cf, val),
        FieldContext::Select(cf) | FieldContext::Radio(cf) => {
            field_types::sub_select_radio(cf, sf, val)
        }
        FieldContext::Date(df) => field_types::sub_date(df, sf, val, ""),
        FieldContext::Relationship(rf) => field_types::sub_relationship(rf, sf),
        FieldContext::Upload(uf) => field_types::sub_upload(uf, sf),
        FieldContext::Array(af) => field_types::sub_array(af, sf, raw_value, indexed_name, opts),
        FieldContext::Blocks(bf) => field_types::sub_blocks(bf, sf, raw_value, indexed_name, opts),
        FieldContext::Group(gf) => field_types::sub_group(gf, sf, raw_value, indexed_name, opts),
        FieldContext::Row(rf) => {
            field_types::sub_row_collapsible_row(rf, sf, raw_value, indexed_name, opts)
        }
        FieldContext::Collapsible(gf) => {
            field_types::sub_row_collapsible_group(gf, sf, raw_value, indexed_name, opts)
        }
        FieldContext::Tabs(tf) => field_types::sub_tabs(tf, sf, raw_value, indexed_name, opts),
        FieldContext::Textarea(tf) => {
            tf.rows = sf.admin.rows.unwrap_or(8);
            tf.resizable = sf.admin.resizable;
        }
        FieldContext::Richtext(rf) => enrich_sub_richtext(rf, sf, indexed_name, opts.errors),
        FieldContext::Text(tf) if sf.has_many => field_types::sub_text_has_many_tags(tf, val),
        FieldContext::Number(nf) if sf.has_many => field_types::sub_number_has_many_tags(nf, val),
        _ => {}
    }
}

/// Build an enriched sub-field context for a single field within an array/blocks row.
/// Constructs the typed [`FieldContext`] variant directly and applies
/// type-specific enrichment via [`dispatch_sub_field_type`].
pub fn build_enriched_sub_field_context(
    sf: &FieldDefinition,
    raw_value: Option<&Value>,
    parent_name: &str,
    idx: usize,
    opts: &SubFieldOpts,
) -> FieldContext {
    let indexed_name = sub_field_indexed_name(sf, parent_name, idx);
    let val = stringify_sub_field_value(raw_value, sf);
    let base = build_sub_field_base(sf, &indexed_name, &val, opts);
    let mut fc = construct_sub_variant(sf, base, &indexed_name);

    if opts.depth < MAX_FIELD_DEPTH {
        dispatch_sub_field_type(&mut fc, sf, &val, raw_value, &indexed_name, opts);
    }

    fc
}

/// Recursively enrich Upload and Relationship sub-field contexts with options from the database.
/// Called for sub-fields inside layout containers (Row, Collapsible, Tabs, Group) and
/// composite fields (Array, Blocks) that can't be enriched during initial context building.
pub fn enrich_nested_fields(
    sub_fields: &mut [FieldContext],
    field_defs: &[FieldDefinition],
    conn: &dyn DbConnection,
    reg: &Registry,
    rel_locale_ctx: Option<&LocaleContext>,
) {
    for (fc, field_def) in sub_fields.iter_mut().zip(field_defs.iter()) {
        match fc {
            FieldContext::Relationship(rf) => {
                enrich_nested_relationship(rf, field_def, conn, reg, rel_locale_ctx);
            }
            FieldContext::Upload(uf) => {
                enrich_nested_upload(uf, field_def, conn, reg, rel_locale_ctx);
            }
            FieldContext::Row(rfld) => {
                enrich_nested_fields(
                    &mut rfld.sub_fields,
                    &field_def.fields,
                    conn,
                    reg,
                    rel_locale_ctx,
                );
            }
            FieldContext::Collapsible(gf) | FieldContext::Group(gf) => {
                enrich_nested_fields(
                    &mut gf.sub_fields,
                    &field_def.fields,
                    conn,
                    reg,
                    rel_locale_ctx,
                );
            }
            FieldContext::Tabs(tf) => {
                for (tab_panel, tab_def) in tf.tabs.iter_mut().zip(field_def.tabs.iter()) {
                    enrich_nested_fields(
                        &mut tab_panel.sub_fields,
                        &tab_def.fields,
                        conn,
                        reg,
                        rel_locale_ctx,
                    );
                }
            }
            FieldContext::Array(af) => {
                enrich_nested_array(af, field_def, conn, reg, rel_locale_ctx);
            }
            FieldContext::Blocks(bf) => {
                enrich_nested_blocks(bf, field_def, conn, reg, rel_locale_ctx);
            }
            _ => {}
        }
    }
}

fn enrich_nested_relationship(
    rf: &mut RelationshipField,
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
    let current_value = rf.base.value.as_str().unwrap_or("");

    if current_value.is_empty() {
        rf.selected_items = Some(Vec::new());
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
        RelationshipSelectedItem {
            id: doc.id.to_string(),
            label,
            ..Default::default()
        }
    });

    rf.selected_items = Some(match item {
        Some(it) => vec![it],
        None => Vec::new(),
    });
}

fn enrich_nested_upload(
    uf: &mut UploadField,
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

    let current_value = uf.base.value.as_str().unwrap_or("");

    if current_value.is_empty() {
        uf.selected_items = Some(Vec::new());
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
        uf.selected_items = Some(Vec::new());
        return;
    };

    if let Some(ref uc) = related_def.upload
        && uc.enabled
    {
        upload::assemble_sizes_object(&mut doc, uc);
    }

    let item = build_upload_item(&doc, &title_field, &admin_thumbnail, true);
    let label = item.label.clone();
    let thumb_url = item.thumbnail_url.clone();

    uf.selected_items = Some(vec![item]);
    uf.selected_filename = Some(label);

    if let Some(url) = thumb_url {
        uf.selected_preview_url = Some(url);
    }
}

fn enrich_nested_array(
    af: &mut ArrayField,
    field_def: &FieldDefinition,
    conn: &dyn DbConnection,
    reg: &Registry,
    rel_locale_ctx: Option<&LocaleContext>,
) {
    // Recurse into array rows' sub-fields
    if let Some(rows) = af.rows.as_mut() {
        for row in rows.iter_mut() {
            enrich_nested_fields(
                &mut row.sub_fields,
                &field_def.fields,
                conn,
                reg,
                rel_locale_ctx,
            );
        }
    }

    // Enrich the <template> sub-fields so new rows added via JS have upload/relationship options
    enrich_nested_fields(
        &mut af.sub_fields,
        &field_def.fields,
        conn,
        reg,
        rel_locale_ctx,
    );
}

fn enrich_nested_blocks(
    bf: &mut BlocksField,
    field_def: &FieldDefinition,
    conn: &dyn DbConnection,
    reg: &Registry,
    rel_locale_ctx: Option<&LocaleContext>,
) {
    // Recurse into block rows' sub-fields, matching each row's block type
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
                    conn,
                    reg,
                    rel_locale_ctx,
                );
            }
        }
    }

    // Enrich block definition templates so new block rows have upload/relationship options
    for (def_ctx, block_def) in bf.block_definitions.iter_mut().zip(field_def.blocks.iter()) {
        enrich_nested_fields(
            &mut def_ctx.fields,
            &block_def.fields,
            conn,
            reg,
            rel_locale_ctx,
        );
    }
}
