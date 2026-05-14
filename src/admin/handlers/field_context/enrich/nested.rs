//! Builds enriched sub-field contexts for array/blocks rows and recursively
//! enriches nested relationship/upload fields with DB-fetched options.

use std::collections::HashMap;

use serde_json::Value;

use super::types::build_upload_item;
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
    core::{FieldDefinition, FieldType, Registry, upload},
    db::{DbConnection, LocaleContext, query},
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
        format!("{parent_name}[{idx}]")
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
    let sf_label = sf.admin.label.as_ref().map_or_else(
        || auto_label_from_name(&sf.name),
        |ls| ls.resolve_default().to_string(),
    );

    // Recompute per-field instead of inheriting from the parent: a localized
    // field inside a non-localized parent must stay editable in non-default
    // locales. Layout wrappers (Row/Tabs/Collapsible) themselves are always
    // non-localized, so they pick up the parent's lock state naturally via
    // this same formula.
    let locale_locked = opts.non_default_locale && !sf.localized;

    BaseFieldData {
        name: indexed_name.to_string(),
        field_name: sf.name.clone(),
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
        readonly: sf.admin.readonly || locale_locked,
        localized: sf.localized,
        locale_locked,
        position: sf.admin.position.clone(),
        template: sf.admin.template.clone(),
        extra: sf.admin.extra.clone(),
        error: opts.errors.get(indexed_name).cloned(),
        validation: ValidationAttrs::default(),
        condition: ConditionData::default(),
    }
}

/// Construct the [`FieldContext`] variant matching `sf.field_type` with
/// `base` populated and per-variant defaults filled in via each variant's
/// `empty(base)` constructor. Type-specific enrichment in
/// [`dispatch_sub_field_type`] subsequently mutates the variant to set its
/// real data.
pub(super) fn construct_sub_variant(
    sf: &FieldDefinition,
    base: BaseFieldData,
    indexed_name: &str,
) -> FieldContext {
    match &sf.field_type {
        FieldType::Text => FieldContext::Text(TextField::empty(base)),
        FieldType::Email => FieldContext::Email(TextField::empty(base)),
        FieldType::Json => FieldContext::Json(TextField::empty(base)),
        FieldType::Textarea => FieldContext::Textarea(TextareaField::empty(base)),
        FieldType::Number => FieldContext::Number(NumberField::empty(base)),
        FieldType::Code => FieldContext::Code(CodeField::empty(base)),
        FieldType::Richtext => FieldContext::Richtext(RichtextField::empty(base)),
        FieldType::Date => FieldContext::Date(DateField::empty(base)),
        FieldType::Checkbox => FieldContext::Checkbox(CheckboxField::empty(base)),
        FieldType::Select => FieldContext::Select(ChoiceField::empty(base)),
        FieldType::Radio => FieldContext::Radio(ChoiceField::empty(base)),
        FieldType::Relationship => FieldContext::Relationship(RelationshipField::empty(base)),
        FieldType::Upload => FieldContext::Upload(UploadField::empty(base)),
        FieldType::Join => FieldContext::Join(JoinField::empty(base)),
        FieldType::Group => FieldContext::Group(GroupField::empty(base)),
        FieldType::Row => FieldContext::Row(RowField::empty(base)),
        FieldType::Collapsible => FieldContext::Collapsible(GroupField::empty(base)),
        FieldType::Tabs => FieldContext::Tabs(TabsField::empty(base)),
        FieldType::Array => {
            FieldContext::Array(ArrayField::empty(base, safe_template_id(indexed_name)))
        }
        FieldType::Blocks => {
            FieldContext::Blocks(BlocksField::empty(base, safe_template_id(indexed_name)))
        }
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
            field_types::sub_select_radio(cf, sf, val);
        }
        FieldContext::Date(df) => field_types::sub_date(df, sf, val, ""),
        FieldContext::Relationship(rf) => field_types::sub_relationship(rf, sf),
        FieldContext::Upload(uf) => field_types::sub_upload(uf, sf),
        FieldContext::Array(af) => field_types::sub_array(af, sf, raw_value, indexed_name, opts),
        FieldContext::Blocks(bf) => field_types::sub_blocks(bf, sf, raw_value, indexed_name, opts),
        FieldContext::Group(gf) => field_types::sub_group(gf, sf, raw_value, indexed_name, opts),
        FieldContext::Row(rf) => {
            field_types::sub_row_collapsible_row(rf, sf, raw_value, indexed_name, opts);
        }
        FieldContext::Collapsible(gf) => {
            field_types::sub_row_collapsible_group(gf, sf, raw_value, indexed_name, opts);
        }
        FieldContext::Tabs(tf) => field_types::sub_tabs(tf, sf, raw_value, indexed_name, opts),
        FieldContext::Textarea(tf) => {
            tf.rows = sf.admin.rows.unwrap_or(8);
            tf.resizable = sf.admin.resizable;
        }
        FieldContext::Richtext(rf) => enrich_sub_richtext(rf, sf, indexed_name, opts.errors),
        FieldContext::Code(cf) => {
            cf.language = sf.admin.language.as_deref().unwrap_or("json").to_string();
            if !sf.admin.languages.is_empty() {
                cf.languages = Some(sf.admin.languages.clone());
            }
        }
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
    let title_field = related_def
        .title_field()
        .map(std::string::ToString::to_string);
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

    let title_field = related_def
        .title_field()
        .map(std::string::ToString::to_string);
    let admin_thumbnail = related_def
        .upload
        .as_ref()
        .and_then(|u| u.admin_thumbnail.clone());

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

    let item = build_upload_item(&doc, title_field.as_ref(), admin_thumbnail.as_ref(), true);
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

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;

    use super::*;
    use crate::{
        admin::handlers::field_context::{
            MAX_FIELD_DEPTH,
            enrich::test_helpers::{
                build_enriched_sub_field_value, enrich_nested_fields_values, make_field,
            },
        },
        core::{
            BlockDefinition, CollectionDefinition, FieldType, LocalizedString, Registry,
            RelationshipConfig, SelectOption, upload::CollectionUpload,
        },
    };

    // ── build_enriched_sub_field_context: composites ─────────────────

    #[test]
    fn enriched_sub_field_nested_array_populates_rows() {
        let mut inner_array = make_field("images", FieldType::Array);
        inner_array.fields = vec![
            make_field("url", FieldType::Text),
            make_field("alt", FieldType::Text),
        ];

        let raw_value = json!([
            {"url": "img1.jpg", "alt": "First"},
            {"url": "img2.jpg", "alt": "Second"},
        ]);

        let ctx = build_enriched_sub_field_value(
            &inner_array,
            Some(&raw_value),
            "content",
            0,
            &SubFieldOpts::builder(&HashMap::new()).depth(1).build(),
        );

        assert_eq!(ctx["field_type"], "array");
        assert_eq!(ctx["row_count"], 2);

        let rows = ctx["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 2);

        let row0_fields = rows[0]["sub_fields"].as_array().unwrap();
        assert_eq!(row0_fields[0]["name"], "content[0][images][0][url]");
        assert_eq!(row0_fields[0]["value"], "img1.jpg");
        assert_eq!(row0_fields[1]["name"], "content[0][images][0][alt]");
        assert_eq!(row0_fields[1]["value"], "First");

        let row1_fields = rows[1]["sub_fields"].as_array().unwrap();
        assert_eq!(row1_fields[0]["value"], "img2.jpg");
        assert_eq!(row1_fields[1]["value"], "Second");

        let template_sub = ctx["sub_fields"].as_array().unwrap();
        assert_eq!(
            template_sub[0]["name"],
            "content[0][images][__INDEX__][url]"
        );
    }

    /// Regression: a Code field directly inside an Array row must inherit
    /// `admin.language` from its definition. Previously `dispatch_sub_field_type`
    /// had no `Code` arm, so the language stayed empty and `CodeMirror` fell back
    /// to the default mode.
    #[test]
    fn enriched_sub_field_code_in_array_row_inherits_admin_language() {
        use crate::core::field::FieldAdminBuilder;

        let mut array = make_field("snippets", FieldType::Array);
        let mut code = make_field("body", FieldType::Code);
        code.admin = FieldAdminBuilder::new()
            .language("javascript".to_string())
            .build();
        array.fields = vec![code];

        let raw_value = json!([{"body": "console.log(1);"}]);

        let ctx = build_enriched_sub_field_value(
            &array,
            Some(&raw_value),
            "doc",
            0,
            &SubFieldOpts::builder(&HashMap::new()).depth(1).build(),
        );

        let rows = ctx["rows"].as_array().unwrap();
        let sub_fields = rows[0]["sub_fields"].as_array().unwrap();
        assert_eq!(sub_fields[0]["field_type"], "code");
        assert_eq!(sub_fields[0]["language"], "javascript");
    }

    /// Regression: a localized field inside a layout wrapper (Row/Tabs/
    /// Collapsible) inside a non-localized Array must remain editable in
    /// non-default locales. The wrapper used to inherit `locale_locked` from
    /// the array verbatim instead of recomputing per child.
    #[test]
    fn localized_field_in_layout_wrapper_in_array_is_editable_in_non_default_locale() {
        let mut row = make_field("layout", FieldType::Row);
        let mut title = make_field("title", FieldType::Text);
        title.localized = true;
        row.fields = vec![title];

        let errors = HashMap::new();
        let opts = SubFieldOpts::builder(&errors)
            .locale_locked(true)
            .non_default_locale(true)
            .depth(1)
            .build();

        let row_value = json!({"title": "Hello"});
        let ctx = build_enriched_sub_field_value(&row, Some(&row_value), "items[0]", 0, &opts);

        assert_eq!(ctx["field_type"], "row");
        assert_eq!(ctx["locale_locked"], true);

        let title_ctx = &ctx["sub_fields"][0];
        assert_eq!(title_ctx["field_name"], "title");
        assert_eq!(title_ctx["localized"], true);
        assert_eq!(
            title_ctx["locale_locked"], false,
            "localized field inside layout wrapper must be unlocked in non-default locale"
        );
        assert_eq!(title_ctx["readonly"], false);
    }

    #[test]
    fn enriched_sub_field_nested_blocks_populates_rows() {
        let mut inner_blocks = make_field("sections", FieldType::Blocks);
        inner_blocks.blocks = vec![{
            let mut bd =
                BlockDefinition::new("text", vec![make_field("body", FieldType::Richtext)]);
            bd.label = Some(LocalizedString::Plain("Text".to_string()));
            bd
        }];

        let raw_value = json!([
            {"_block_type": "text", "body": "<p>Hello</p>"},
        ]);

        let ctx = build_enriched_sub_field_value(
            &inner_blocks,
            Some(&raw_value),
            "page",
            2,
            &SubFieldOpts::builder(&HashMap::new()).depth(1).build(),
        );

        assert_eq!(ctx["field_type"], "blocks");
        assert_eq!(ctx["row_count"], 1);

        let rows = ctx["rows"].as_array().unwrap();
        assert_eq!(rows[0]["_block_type"], "text");
        assert_eq!(rows[0]["block_label"], "Text");

        let sub_fields = rows[0]["sub_fields"].as_array().unwrap();
        assert_eq!(sub_fields[0]["name"], "page[2][sections][0][body]");
        assert_eq!(sub_fields[0]["value"], "<p>Hello</p>");

        let block_defs = ctx["block_definitions"].as_array().unwrap();
        assert_eq!(block_defs.len(), 1);
    }

    /// Per-field `admin.template` + `admin.extra` survive the nested-field
    /// enrichment path. Builds a deeply-nested rating field — group → array
    /// → number with `admin.template = "fields/rating"` — and verifies the
    /// enriched sub-field context still carries `template` and `extra` at
    /// the top level so `RenderFieldHelper` can route it.
    #[test]
    fn enriched_sub_field_preserves_admin_template_and_extra_when_nested() {
        use crate::core::field::FieldAdminBuilder;

        let mut rating = make_field("rating", FieldType::Number);
        rating.admin = FieldAdminBuilder::new()
            .template("fields/rating")
            .extra_insert("color", "amber")
            .extra_insert("max_stars", 5_i64)
            .build();

        let mut reviews_array = make_field("reviews", FieldType::Array);
        reviews_array.fields = vec![rating];
        let mut outer_group = make_field("section", FieldType::Group);
        outer_group.fields = vec![reviews_array];

        let raw_value = json!({
            "reviews": [
                { "rating": "4" },
                { "rating": "5" },
            ],
        });

        let ctx = build_enriched_sub_field_value(
            &outer_group,
            Some(&raw_value),
            "items",
            0,
            &SubFieldOpts::builder(&HashMap::new()).depth(1).build(),
        );

        let group_subs = ctx["sub_fields"].as_array().unwrap();
        let arr = &group_subs[0];
        assert_eq!(arr["field_type"], "array");
        let rows = arr["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 2, "two array rows from raw_value");

        for (i, row) in rows.iter().enumerate() {
            let row_fields = row["sub_fields"].as_array().unwrap();
            let rating_ctx = &row_fields[0];
            assert_eq!(rating_ctx["field_name"], "rating", "row {i}");
            assert_eq!(
                rating_ctx["template"], "fields/rating",
                "row {i}: template must survive nested enrichment so RenderFieldHelper picks it up",
            );
            assert_eq!(
                rating_ctx["extra"]["color"], "amber",
                "row {i}: extra.color must survive nested enrichment",
            );
            assert_eq!(rating_ctx["extra"]["max_stars"], 5, "row {i}");
        }
    }

    #[test]
    fn enriched_sub_field_nested_group_populates_values() {
        let mut inner_group = make_field("meta", FieldType::Group);
        inner_group.fields = vec![
            make_field("author", FieldType::Text),
            make_field("published", FieldType::Checkbox),
        ];

        let raw_value = json!({
            "author": "Alice",
            "published": "1",
        });

        let ctx = build_enriched_sub_field_value(
            &inner_group,
            Some(&raw_value),
            "items",
            0,
            &SubFieldOpts::builder(&HashMap::new()).depth(1).build(),
        );

        assert_eq!(ctx["field_type"], "group");
        let sub_fields = ctx["sub_fields"].as_array().unwrap();
        assert_eq!(sub_fields.len(), 2);
        assert_eq!(sub_fields[0]["name"], "items[0][meta][0][author]");
        assert_eq!(sub_fields[0]["value"], "Alice");
        assert_eq!(sub_fields[1]["name"], "items[0][meta][0][published]");
        assert_eq!(sub_fields[1]["checked"], true);
    }

    #[test]
    fn enriched_sub_field_empty_nested_array() {
        let mut inner_array = make_field("tags", FieldType::Array);
        inner_array.fields = vec![make_field("name", FieldType::Text)];

        let ctx = build_enriched_sub_field_value(
            &inner_array,
            None,
            "items",
            0,
            &SubFieldOpts::builder(&HashMap::new()).depth(1).build(),
        );

        assert_eq!(ctx["field_type"], "array");
        assert_eq!(ctx["row_count"], 0);
        let rows = ctx["rows"].as_array().unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn enriched_sub_field_select_preserves_selected() {
        let mut select_field = make_field("status", FieldType::Select);
        select_field.options = vec![
            SelectOption::new(LocalizedString::Plain("Draft".to_string()), "draft"),
            SelectOption::new(LocalizedString::Plain("Published".to_string()), "published"),
        ];

        let raw_value = json!("published");

        let ctx = build_enriched_sub_field_value(
            &select_field,
            Some(&raw_value),
            "items",
            0,
            &SubFieldOpts::builder(&HashMap::new()).depth(1).build(),
        );

        let opts = ctx["options"].as_array().unwrap();
        assert_eq!(opts[0]["selected"], false);
        assert_eq!(opts[1]["selected"], true);
    }

    // ── build_enriched_sub_field_context: error + max depth ──────────

    #[test]
    fn enriched_sub_field_with_error() {
        let sf = make_field("title", FieldType::Text);
        let mut errors = HashMap::new();
        errors.insert("content[0][title]".to_string(), "Required".to_string());
        let ctx = build_enriched_sub_field_value(
            &sf,
            Some(&json!("val")),
            "content",
            0,
            &SubFieldOpts::builder(&errors).depth(1).build(),
        );
        assert_eq!(ctx["error"], "Required");
    }

    #[test]
    fn enriched_sub_field_max_depth_returns_early() {
        let mut arr = make_field("deep", FieldType::Array);
        arr.fields = vec![make_field("leaf", FieldType::Text)];
        let ctx = build_enriched_sub_field_value(
            &arr,
            Some(&json!([])),
            "parent",
            0,
            &SubFieldOpts::builder(&HashMap::new())
                .depth(MAX_FIELD_DEPTH)
                .build(),
        );
        assert!(ctx.get("rows").is_none());
        assert_eq!(
            ctx.get("sub_fields")
                .and_then(|v| v.as_array())
                .map_or(0, std::vec::Vec::len),
            0
        );
    }

    // ── build_enriched_sub_field_context: scalars ────────────────────

    #[test]
    fn enriched_sub_field_date_day_only() {
        let sf = make_field("d", FieldType::Date);
        let raw = json!("2026-03-15T10:00:00Z");
        let ctx = build_enriched_sub_field_value(
            &sf,
            Some(&raw),
            "items",
            0,
            &SubFieldOpts::builder(&HashMap::new()).depth(1).build(),
        );
        assert_eq!(ctx["picker_appearance"], "dayOnly");
        assert_eq!(ctx["date_only_value"], "2026-03-15");
    }

    #[test]
    fn enriched_sub_field_date_day_and_time() {
        let mut sf = make_field("d", FieldType::Date);
        sf.picker_appearance = Some("dayAndTime".to_string());
        let raw = json!("2026-03-15T10:30:00Z");
        let ctx = build_enriched_sub_field_value(
            &sf,
            Some(&raw),
            "items",
            0,
            &SubFieldOpts::builder(&HashMap::new()).depth(1).build(),
        );
        assert_eq!(ctx["picker_appearance"], "dayAndTime");
        assert_eq!(ctx["datetime_local_value"], "2026-03-15T10:30");
    }

    #[test]
    fn enriched_sub_field_date_short_value() {
        let sf = make_field("d", FieldType::Date);
        let raw = json!("short");
        let ctx = build_enriched_sub_field_value(
            &sf,
            Some(&raw),
            "items",
            0,
            &SubFieldOpts::builder(&HashMap::new()).depth(1).build(),
        );
        assert_eq!(ctx["date_only_value"], "short");
    }

    #[test]
    fn enriched_sub_field_upload() {
        let mut sf = make_field("image", FieldType::Upload);
        sf.relationship = Some(RelationshipConfig::new("media", false));
        let ctx = build_enriched_sub_field_value(
            &sf,
            Some(&json!("img123")),
            "items",
            0,
            &SubFieldOpts::builder(&HashMap::new()).depth(1).build(),
        );
        assert_eq!(ctx["relationship_collection"], "media");
        assert_eq!(ctx["picker"], "drawer");
    }

    #[test]
    fn enriched_sub_field_relationship() {
        let mut sf = make_field("author", FieldType::Relationship);
        sf.relationship = Some(RelationshipConfig::new("users", true));
        let ctx = build_enriched_sub_field_value(
            &sf,
            Some(&json!("user1")),
            "items",
            0,
            &SubFieldOpts::builder(&HashMap::new()).depth(1).build(),
        );
        assert_eq!(ctx["relationship_collection"], "users");
        assert_eq!(ctx["has_many"], true);
    }

    #[test]
    fn enriched_sub_field_null_value_empty_string() {
        let sf = make_field("title", FieldType::Text);
        let ctx = build_enriched_sub_field_value(
            &sf,
            Some(&serde_json::Value::Null),
            "items",
            0,
            &SubFieldOpts::builder(&HashMap::new()).depth(1).build(),
        );
        assert_eq!(ctx["value"], "");
    }

    #[test]
    fn enriched_sub_field_number_to_string() {
        let sf = make_field("count", FieldType::Number);
        let ctx = build_enriched_sub_field_value(
            &sf,
            Some(&json!(42)),
            "items",
            0,
            &SubFieldOpts::builder(&HashMap::new()).depth(1).build(),
        );
        assert_eq!(ctx["value"], "42");
    }

    #[test]
    fn enriched_sub_field_no_value() {
        let sf = make_field("title", FieldType::Text);
        let ctx = build_enriched_sub_field_value(
            &sf,
            None,
            "items",
            0,
            &SubFieldOpts::builder(&HashMap::new()).depth(1).build(),
        );
        assert_eq!(ctx["value"], "");
    }

    // ── build_enriched_sub_field_context: array/blocks options ───────

    #[test]
    fn enriched_sub_field_array_with_options() {
        let mut arr = make_field("tags", FieldType::Array);
        arr.fields = vec![make_field("name", FieldType::Text)];
        arr.min_rows = Some(1);
        arr.max_rows = Some(5);
        arr.admin.collapsed = true;
        arr.admin.labels_singular = Some(LocalizedString::Plain("Tag".to_string()));
        let ctx = build_enriched_sub_field_value(
            &arr,
            Some(&json!([])),
            "items",
            0,
            &SubFieldOpts::builder(&HashMap::new()).depth(1).build(),
        );
        assert_eq!(ctx["min_rows"], 1);
        assert_eq!(ctx["max_rows"], 5);
        assert_eq!(ctx["init_collapsed"], true);
        assert_eq!(ctx["add_label"], "Tag");
    }

    #[test]
    fn enriched_sub_field_blocks_with_options() {
        let mut blk = make_field("sections", FieldType::Blocks);
        blk.blocks = vec![BlockDefinition::new(
            "text",
            vec![make_field("body", FieldType::Text)],
        )];
        blk.min_rows = Some(0);
        blk.max_rows = Some(10);
        blk.admin.collapsed = true;
        blk.admin.labels_singular = Some(LocalizedString::Plain("Section".to_string()));
        blk.admin.label_field = Some("body".to_string());
        let ctx = build_enriched_sub_field_value(
            &blk,
            Some(&json!([])),
            "items",
            0,
            &SubFieldOpts::builder(&HashMap::new()).depth(1).build(),
        );
        assert_eq!(ctx["min_rows"], 0);
        assert_eq!(ctx["max_rows"], 10);
        assert_eq!(ctx["init_collapsed"], true);
        assert_eq!(ctx["add_label"], "Section");
        assert_eq!(ctx["label_field"], "body");
    }

    #[test]
    fn enriched_sub_field_nested_array_row_errors() {
        let mut inner_array = make_field("items", FieldType::Array);
        inner_array.fields = vec![make_field("title", FieldType::Text)];

        let raw_value = json!([{"title": ""}]);
        let mut errors = HashMap::new();
        errors.insert(
            "parent[0][items][0][title]".to_string(),
            "Required".to_string(),
        );

        let ctx = build_enriched_sub_field_value(
            &inner_array,
            Some(&raw_value),
            "parent",
            0,
            &SubFieldOpts::builder(&errors).depth(1).build(),
        );

        let rows = ctx["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        let row_fields = rows[0]["sub_fields"].as_array().unwrap();
        assert_eq!(row_fields[0]["error"], "Required");
        assert_eq!(rows[0]["has_errors"], true);
    }

    #[test]
    fn enriched_sub_field_nested_blocks_row_errors() {
        let mut blk = make_field("sections", FieldType::Blocks);
        blk.blocks = vec![{
            let mut bd =
                BlockDefinition::new("text", vec![make_field("body", FieldType::Richtext)]);
            bd.label = Some(LocalizedString::Plain("Text".to_string()));
            bd
        }];

        let raw_value = json!([{"_block_type": "text", "body": ""}]);
        let mut errors = HashMap::new();
        errors.insert(
            "parent[0][sections][0][body]".to_string(),
            "Required".to_string(),
        );

        let ctx = build_enriched_sub_field_value(
            &blk,
            Some(&raw_value),
            "parent",
            0,
            &SubFieldOpts::builder(&errors).depth(1).build(),
        );

        let rows = ctx["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["has_errors"], true);
    }

    #[test]
    fn enriched_sub_field_group_collapsed() {
        let mut grp = make_field("meta", FieldType::Group);
        grp.fields = vec![make_field("author", FieldType::Text)];
        grp.admin.collapsed = true;
        let raw = json!({"author": "Alice"});
        let ctx = build_enriched_sub_field_value(
            &grp,
            Some(&raw),
            "items",
            0,
            &SubFieldOpts::builder(&HashMap::new()).depth(1).build(),
        );
        assert_eq!(ctx["collapsed"], true);
    }

    #[test]
    fn enriched_sub_field_group_with_null_value() {
        let mut grp = make_field("meta", FieldType::Group);
        grp.fields = vec![make_field("author", FieldType::Text)];
        let ctx = build_enriched_sub_field_value(
            &grp,
            Some(&serde_json::Value::Null),
            "items",
            0,
            &SubFieldOpts::builder(&HashMap::new()).depth(1).build(),
        );
        let sub_fields = ctx["sub_fields"].as_array().unwrap();
        assert_eq!(sub_fields[0]["value"], "");
    }

    #[test]
    fn enriched_sub_field_nested_blocks_unknown_type() {
        let mut blk = make_field("sections", FieldType::Blocks);
        blk.blocks = vec![{
            let mut bd =
                BlockDefinition::new("text", vec![make_field("body", FieldType::Richtext)]);
            bd.label = Some(LocalizedString::Plain("Text".to_string()));
            bd
        }];

        let raw_value = json!([{"_block_type": "unknown_type", "body": "content"}]);

        let ctx = build_enriched_sub_field_value(
            &blk,
            Some(&raw_value),
            "parent",
            0,
            &SubFieldOpts::builder(&HashMap::new()).depth(1).build(),
        );

        let rows = ctx["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["_block_type"], "unknown_type");
        // Falls back to the block_type string when the def is missing.
        assert_eq!(rows[0]["block_label"], "unknown_type");
        let sub_fields = rows[0]["sub_fields"].as_array().unwrap();
        assert!(sub_fields.is_empty());
    }

    // ── enrich_nested_fields: relationships, uploads, recursion ──────

    #[test]
    fn enrich_nested_fields_upload_gets_options() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE media (
                id TEXT PRIMARY KEY,
                alt TEXT,
                caption TEXT,
                filename TEXT,
                mime_type TEXT,
                url TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            INSERT INTO media (id, alt, filename, mime_type, url, created_at, updated_at)
            VALUES ('img1', 'Logo', 'logo.png', 'image/png', '/uploads/media/logo.png', '2024-01-01', '2024-01-01');
            INSERT INTO media (id, alt, filename, mime_type, url, created_at, updated_at)
            VALUES ('img2', 'Banner', 'banner.jpg', 'image/jpeg', '/uploads/media/banner.jpg', '2024-01-01', '2024-01-01');"
        ).unwrap();

        let mut media_def = CollectionDefinition::new("media");
        media_def.timestamps = true;
        media_def.fields = vec![
            make_field("alt", FieldType::Text),
            make_field("caption", FieldType::Text),
            make_field("filename", FieldType::Text),
            make_field("mime_type", FieldType::Text),
            make_field("url", FieldType::Text),
        ];
        media_def.upload = Some(CollectionUpload {
            enabled: true,
            mime_types: vec!["image/*".to_string()],
            ..Default::default()
        });

        let mut registry = Registry::new();
        registry.register_collection(media_def);

        let mut upload_field = make_field("image", FieldType::Upload);
        upload_field.relationship = Some(RelationshipConfig::new("media", false));

        let field_defs = vec![upload_field];
        let mut sub_fields = vec![json!({
            "name": "content[0][image]",
            "field_type": "upload",
            "value": "img1",
            "relationship_collection": "media",
        })];

        enrich_nested_fields_values(&mut sub_fields, &field_defs, &conn, &registry, None);

        let items = sub_fields[0]["selected_items"]
            .as_array()
            .expect("selected_items should be populated");
        assert_eq!(items.len(), 1, "Should have 1 selected item");
        assert_eq!(items[0]["id"], "img1");
        assert_eq!(items[0]["label"], "logo.png");
    }

    #[test]
    fn enrich_nested_fields_relationship_gets_options() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE users (
                id TEXT PRIMARY KEY,
                name TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            INSERT INTO users (id, name, created_at, updated_at)
            VALUES ('u1', 'Alice', '2024-01-01', '2024-01-01');
            INSERT INTO users (id, name, created_at, updated_at)
            VALUES ('u2', 'Bob', '2024-01-01', '2024-01-01');",
        )
        .unwrap();

        let mut users_def = CollectionDefinition::new("users");
        users_def.timestamps = true;
        users_def.fields = vec![make_field("name", FieldType::Text)];
        users_def.admin.use_as_title = Some("name".to_string());

        let mut registry = Registry::new();
        registry.register_collection(users_def);

        let mut rel_field = make_field("author", FieldType::Relationship);
        rel_field.relationship = Some(RelationshipConfig::new("users", false));

        let field_defs = vec![rel_field];
        let mut sub_fields = vec![json!({
            "name": "items[0][author]",
            "field_type": "relationship",
            "value": "u1",
            "relationship_collection": "users",
        })];

        enrich_nested_fields_values(&mut sub_fields, &field_defs, &conn, &registry, None);

        let items = sub_fields[0]["selected_items"]
            .as_array()
            .expect("selected_items should be populated");
        assert_eq!(items.len(), 1, "Should have 1 selected item");
        assert_eq!(items[0]["id"], "u1");
        assert_eq!(items[0]["label"], "Alice");
    }

    #[test]
    fn enrich_nested_fields_recurses_into_layout() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE tags (
                id TEXT PRIMARY KEY,
                label TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            INSERT INTO tags (id, label, created_at, updated_at)
            VALUES ('t1', 'Rust', '2024-01-01', '2024-01-01');",
        )
        .unwrap();

        let mut tags_def = CollectionDefinition::new("tags");
        tags_def.timestamps = true;
        tags_def.fields = vec![make_field("label", FieldType::Text)];
        tags_def.admin.use_as_title = Some("label".to_string());

        let mut registry = Registry::new();
        registry.register_collection(tags_def);

        let mut rel_field = make_field("tag", FieldType::Relationship);
        rel_field.relationship = Some(RelationshipConfig::new("tags", false));
        let row_field = FieldDefinition::builder("row1", FieldType::Row)
            .fields(vec![rel_field])
            .build();

        let field_defs = vec![row_field];
        let mut sub_fields = vec![json!({
            "name": "row1",
            "field_type": "row",
            "sub_fields": [{
                "name": "tag",
                "field_type": "relationship",
                "value": "",
                "relationship_collection": "tags",
            }],
        })];

        enrich_nested_fields_values(&mut sub_fields, &field_defs, &conn, &registry, None);

        let row_subs = sub_fields[0]["sub_fields"].as_array().unwrap();
        let items = row_subs[0]["selected_items"]
            .as_array()
            .expect("Nested relationship inside Row should be enriched");
        assert_eq!(
            items.len(),
            0,
            "Empty value should produce empty selected_items"
        );
    }

    /// Regression: block-definition templates (used to render new rows) must
    /// have their upload fields enriched with `selected_items` context.
    #[test]
    fn enrich_nested_fields_blocks_template_gets_upload_options() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE media (
                id TEXT PRIMARY KEY,
                filename TEXT,
                mime_type TEXT,
                url TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            INSERT INTO media (id, filename, mime_type, url, created_at, updated_at)
            VALUES ('m1', 'photo.jpg', 'image/jpeg', '/uploads/photo.jpg', '2024-01-01', '2024-01-01');"
        ).unwrap();

        let mut media_def = CollectionDefinition::new("media");
        media_def.timestamps = true;
        media_def.fields = vec![
            make_field("filename", FieldType::Text),
            make_field("mime_type", FieldType::Text),
            make_field("url", FieldType::Text),
        ];
        media_def.upload = Some(CollectionUpload {
            enabled: true,
            ..Default::default()
        });

        let mut registry = Registry::new();
        registry.register_collection(media_def);

        let mut upload_field = make_field("image", FieldType::Upload);
        upload_field.relationship = Some(RelationshipConfig::new("media", false));
        let mut blocks_field = FieldDefinition::builder("content", FieldType::Blocks).build();
        blocks_field.blocks = vec![BlockDefinition::new("image", vec![upload_field])];

        let field_defs = vec![blocks_field];
        let mut sub_fields = vec![json!({
            "name": "content",
            "field_type": "blocks",
            "block_definitions": [{
                "block_type": "image",
                "label": "Image",
                "fields": [{
                    "name": "content[__INDEX__][image]",
                    "field_type": "upload",
                    "value": "",
                    "relationship_collection": "media",
                }],
            }],
            "rows": [],
        })];

        enrich_nested_fields_values(&mut sub_fields, &field_defs, &conn, &registry, None);

        let block_defs = sub_fields[0]["block_definitions"].as_array().unwrap();
        let fields = block_defs[0]["fields"].as_array().unwrap();
        let items = fields[0]["selected_items"]
            .as_array()
            .expect("Upload inside block template should have selected_items");
        assert_eq!(
            items.len(),
            0,
            "Empty value should produce empty selected_items"
        );
    }

    /// Regression: array-template sub-fields (used for the new-row UI) must
    /// have upload `selected_items` enriched.
    #[test]
    fn enrich_nested_fields_array_template_gets_upload_options() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE media (
                id TEXT PRIMARY KEY,
                filename TEXT,
                mime_type TEXT,
                url TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            INSERT INTO media (id, filename, mime_type, url, created_at, updated_at)
            VALUES ('m1', 'doc.pdf', 'application/pdf', '/uploads/doc.pdf', '2024-01-01', '2024-01-01');"
        ).unwrap();

        let mut media_def = CollectionDefinition::new("media");
        media_def.timestamps = true;
        media_def.fields = vec![
            make_field("filename", FieldType::Text),
            make_field("mime_type", FieldType::Text),
            make_field("url", FieldType::Text),
        ];
        media_def.upload = Some(CollectionUpload {
            enabled: true,
            ..Default::default()
        });

        let mut registry = Registry::new();
        registry.register_collection(media_def);

        let mut upload_field = make_field("file", FieldType::Upload);
        upload_field.relationship = Some(RelationshipConfig::new("media", false));
        let array_field = FieldDefinition::builder("attachments", FieldType::Array)
            .fields(vec![upload_field])
            .build();

        let field_defs = vec![array_field];
        let mut sub_fields = vec![json!({
            "name": "attachments",
            "field_type": "array",
            "sub_fields": [{
                "name": "attachments[__INDEX__][file]",
                "field_type": "upload",
                "value": "",
                "relationship_collection": "media",
            }],
            "rows": [],
        })];

        enrich_nested_fields_values(&mut sub_fields, &field_defs, &conn, &registry, None);

        let template_fields = sub_fields[0]["sub_fields"].as_array().unwrap();
        let items = template_fields[0]["selected_items"]
            .as_array()
            .expect("Upload inside array template should have selected_items");
        assert_eq!(
            items.len(),
            0,
            "Empty value should produce empty selected_items"
        );
    }
}
