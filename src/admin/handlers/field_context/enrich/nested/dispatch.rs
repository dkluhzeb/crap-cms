//! Constructs the typed [`FieldContext`] variant of a sub-field and applies
//! its type-specific enrichment (options, rows, dates, tags, editors).

use std::collections::HashMap;

use serde_json::Value;

use crate::{
    admin::{
        context::field::{
            ArrayField, BaseFieldData, BlocksField, CheckboxField, ChoiceField, CodeField,
            DateField, FieldContext, GroupField, JoinField, JsonField, NumberField,
            RelationshipField, RichtextField, RowField, TabsField, TextField, TextareaField,
            UploadField,
        },
        handlers::field_context::{
            collect_node_attr_errors,
            enrich::{SubFieldOpts, field_types},
            safe_template_id,
        },
    },
    core::{FieldDefinition, FieldType},
};

/// Construct the [`FieldContext`] variant matching `sf.field_type` with
/// `base` populated and per-variant defaults filled in via each variant's
/// `empty(base)` constructor. Type-specific enrichment in
/// [`dispatch_sub_field_type`] subsequently mutates the variant to set its
/// real data.
pub(in crate::admin::handlers::field_context::enrich) fn construct_sub_variant(
    sf: &FieldDefinition,
    base: BaseFieldData,
    indexed_name: &str,
) -> FieldContext {
    match &sf.field_type {
        FieldType::Text => FieldContext::Text(TextField::empty(base)),
        FieldType::Email => FieldContext::Email(TextField::empty(base)),
        FieldType::Json => FieldContext::Json(JsonField::new(base, sf.admin.rows)),
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
pub(in crate::admin::handlers::field_context::enrich) fn enrich_sub_richtext(
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
pub(super) fn dispatch_sub_field_type(
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
            cf.rows = sf.admin.rows;
            if !sf.admin.languages.is_empty() {
                cf.languages = Some(sf.admin.languages.clone());
            }
        }
        FieldContext::Text(tf) if sf.has_many => field_types::sub_text_has_many_tags(tf, val),
        FieldContext::Number(nf) if sf.has_many => field_types::sub_number_has_many_tags(nf, val),
        _ => {}
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{
        admin::handlers::field_context::enrich::test_helpers::{
            build_enriched_sub_field_value, make_field,
        },
        core::{
            BlockDefinition, FieldAdminBuilder, LocalizedString, PickerAppearance,
            RelationshipConfig, SelectOption,
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

    /// A row's stored value the field no longer declares renders like a
    /// top-level one: selected and marked unlisted, so re-saving the row keeps
    /// it — for a single select and for each element of a `has_many` one.
    #[test]
    fn enriched_sub_field_select_keeps_a_retired_value_selected() {
        let choice = |name: &str, has_many: bool| {
            FieldDefinition::builder(name, FieldType::Select)
                .has_many(has_many)
                .options(vec![SelectOption::new(
                    LocalizedString::Plain("Draft".to_string()),
                    "draft",
                )])
                .build()
        };
        let errors = HashMap::new();
        let opts = SubFieldOpts::builder(&errors).depth(1).build();

        let single = build_enriched_sub_field_value(
            &choice("status", false),
            Some(&json!("legacy")),
            "items",
            0,
            &opts,
        );
        let many = build_enriched_sub_field_value(
            &choice("tags", true),
            Some(&json!(["draft", "legacy"])),
            "items",
            0,
            &opts,
        );

        for ctx in [single, many] {
            let retired = &ctx["options"][1];
            assert_eq!(retired["value"], "legacy");
            assert_eq!(retired["selected"], true);
            assert_eq!(retired["unlisted"], true);
        }
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
        sf.picker_appearance = Some(PickerAppearance::DayAndTime);
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

    // ── build_enriched_sub_field_context: array/blocks options ───────

    #[test]
    fn enriched_sub_field_array_with_options() {
        let mut arr = make_field("tags", FieldType::Array);
        arr.fields = vec![make_field("name", FieldType::Text)];
        arr.min_rows = Some(1);
        arr.max_rows = Some(5);
        arr.admin.collapsed = true;
        arr.admin.labels.singular = Some(LocalizedString::Plain("Tag".to_string()));
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
        blk.admin.labels.singular = Some(LocalizedString::Plain("Section".to_string()));
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
}
