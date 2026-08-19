//! Top-level field context building — select options and field list assembly.

use std::collections::{HashMap, HashSet};

use serde_json::from_str;

use crate::{
    admin::context::field::{FieldContext, SelectOption},
    core::FieldDefinition,
};

use super::single::build_single_field_context;

/// Build select/radio options with `selected` flags, handling both single and
/// multi-select. Returns `(options, is_has_many)`.
pub(in crate::admin::handlers::field_context) fn build_select_options(
    field: &FieldDefinition,
    value: &str,
) -> (Vec<SelectOption>, bool) {
    if field.has_many {
        let selected_values: HashSet<String> = from_str(value).unwrap_or_default();

        let options: Vec<_> = field
            .options
            .iter()
            .map(|opt| SelectOption {
                label: opt.label.resolve_default().to_string(),
                value: opt.value.clone(),
                selected: selected_values.contains(&opt.value),
            })
            .collect();

        (options, true)
    } else {
        let options: Vec<_> = field
            .options
            .iter()
            .map(|opt| SelectOption {
                label: opt.label.resolve_default().to_string(),
                value: opt.value.clone(),
                selected: opt.value == value,
            })
            .collect();

        (options, false)
    }
}

/// Build field context objects for template rendering.
///
/// `filter_hidden`: when true, fields with `admin.hidden = true` are skipped
/// (form rendering — false during error re-renders so the user's entered
/// values are preserved across the round-trip).
///
/// Fields with top-level `hidden = true` are *always* skipped — the data is
/// stripped from API responses, so there's nothing to render and no value to
/// preserve. The `filter_hidden` toggle does not apply.
///
/// `non_default_locale`: when true, non-localized fields are rendered readonly
/// (locked) because they are shared across all locales and should only be
/// edited from the default locale.
///
/// Returns `Vec<FieldContext>` — typed end-to-end. Downstream consumers
/// (`enrich_field_contexts`, `apply_display_conditions`,
/// `split_sidebar_fields`, the `fields` field on each typed page context)
/// all consume typed values; serialization to `serde_json::Value` happens
/// once when the page context is serialized for the `before_render` hook.
pub fn build_field_contexts(
    fields: &[FieldDefinition],
    values: &HashMap<String, String>,
    errors: &HashMap<String, String>,
    filter_hidden: bool,
    non_default_locale: bool,
) -> Vec<FieldContext> {
    visible_field_defs(fields, filter_hidden)
        .map(|field| build_single_field_context(field, values, errors, "", non_default_locale, 0))
        .collect()
}

/// The top-level field defs that become form fields, in order — the **single
/// source of truth** for "which fields are visible." Shared by
/// [`build_field_contexts`], `apply_display_conditions`, and
/// `enrich_field_contexts` so their per-field `zip`s stay aligned: `field.hidden`
/// is always dropped, `admin.hidden` only when `filter_hidden`. Duplicating this
/// filter (as these three sites used to) silently desyncs the zips when a
/// top-level hidden field shifts the pairing.
pub(in crate::admin::handlers::field_context) fn visible_field_defs(
    field_defs: &[FieldDefinition],
    filter_hidden: bool,
) -> impl Iterator<Item = &FieldDefinition> {
    field_defs
        .iter()
        .filter(|f| !f.hidden)
        .filter(move |f| !filter_hidden || !f.admin.hidden)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        admin::handlers::field_context::test_helpers::{build_value_contexts, make_field},
        core::field::{BlockDefinition, FieldTab, FieldType, LocalizedString, RelationshipConfig},
    };

    // ── Array / Blocks sub-field enrichment ───────────────────────────

    #[test]
    fn build_field_contexts_array_sub_fields_include_type_and_label() {
        let mut arr_field = make_field("items", FieldType::Array);
        arr_field.fields = vec![
            make_field("title", FieldType::Text),
            make_field("body", FieldType::Richtext),
        ];
        let fields = vec![arr_field];
        let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
        assert_eq!(result.len(), 1);
        let sub_fields = result[0]["sub_fields"].as_array().unwrap();
        assert_eq!(sub_fields.len(), 2);
        assert_eq!(sub_fields[0]["field_type"], "text");
        assert_eq!(sub_fields[0]["label"], "Title");
        assert_eq!(sub_fields[1]["field_type"], "richtext");
        assert_eq!(sub_fields[1]["label"], "Body");
    }

    #[test]
    fn build_field_contexts_array_select_sub_field_includes_options() {
        let mut select_sf = make_field("status", FieldType::Select);
        select_sf.options = vec![
            crate::core::SelectOption::new(LocalizedString::Plain("Draft".to_string()), "draft"),
            crate::core::SelectOption::new(
                LocalizedString::Plain("Published".to_string()),
                "published",
            ),
        ];
        let mut arr_field = make_field("items", FieldType::Array);
        arr_field.fields = vec![select_sf];
        let fields = vec![arr_field];
        let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
        let sub_fields = result[0]["sub_fields"].as_array().unwrap();
        let opts = sub_fields[0]["options"].as_array().unwrap();
        assert_eq!(opts.len(), 2);
        assert_eq!(opts[0]["value"], "draft");
        assert_eq!(opts[1]["value"], "published");
    }

    #[test]
    fn build_field_contexts_blocks_sub_fields_include_type_and_label() {
        let mut blocks_field = make_field("content", FieldType::Blocks);
        blocks_field.blocks = vec![{
            let mut bd = BlockDefinition::new(
                "rich",
                vec![
                    make_field("heading", FieldType::Text),
                    make_field("body", FieldType::Richtext),
                ],
            );
            bd.label = Some(LocalizedString::Plain("Rich Text".to_string()));
            bd
        }];
        let fields = vec![blocks_field];
        let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
        let block_defs = result[0]["block_definitions"].as_array().unwrap();
        assert_eq!(block_defs.len(), 1);
        let block_fields = block_defs[0]["fields"].as_array().unwrap();
        assert_eq!(block_fields.len(), 2);
        assert_eq!(block_fields[0]["field_type"], "text");
        assert_eq!(block_fields[0]["label"], "Heading");
        assert_eq!(block_fields[1]["field_type"], "richtext");
        assert_eq!(block_fields[1]["label"], "Body");
    }

    #[test]
    fn build_field_contexts_blocks_select_sub_field_includes_options() {
        let mut select_sf = make_field("align", FieldType::Select);
        select_sf.options = vec![
            crate::core::SelectOption::new(LocalizedString::Plain("Left".to_string()), "left"),
            crate::core::SelectOption::new(LocalizedString::Plain("Center".to_string()), "center"),
        ];
        let mut blocks_field = make_field("layout", FieldType::Blocks);
        blocks_field.blocks = vec![BlockDefinition::new("section", vec![select_sf])];
        let fields = vec![blocks_field];
        let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
        let block_defs = result[0]["block_definitions"].as_array().unwrap();
        let block_fields = block_defs[0]["fields"].as_array().unwrap();
        let opts = block_fields[0]["options"].as_array().unwrap();
        assert_eq!(opts.len(), 2);
        assert_eq!(opts[0]["value"], "left");
        assert_eq!(opts[1]["value"], "center");
    }

    // ── Date ──────────────────────────────────────────────────────────

    #[test]
    fn build_field_contexts_date_default_day_only() {
        let date_field = make_field("published_at", FieldType::Date);
        let fields = vec![date_field];
        let mut values = HashMap::new();
        values.insert(
            "published_at".to_string(),
            "2026-01-15T12:00:00.000Z".to_string(),
        );
        let result = build_value_contexts(&fields, &values, &HashMap::new(), false, false);
        assert_eq!(result[0]["picker_appearance"], "dayOnly");
        assert_eq!(result[0]["date_only_value"], "2026-01-15");
    }

    #[test]
    fn build_field_contexts_date_day_and_time() {
        let mut date_field = make_field("event_at", FieldType::Date);
        date_field.picker_appearance = Some(crate::core::PickerAppearance::DayAndTime);
        let fields = vec![date_field];
        let mut values = HashMap::new();
        values.insert(
            "event_at".to_string(),
            "2026-01-15T09:30:00.000Z".to_string(),
        );
        let result = build_value_contexts(&fields, &values, &HashMap::new(), false, false);
        assert_eq!(result[0]["picker_appearance"], "dayAndTime");
        assert_eq!(result[0]["datetime_local_value"], "2026-01-15T09:30");
    }

    #[test]
    fn build_field_contexts_date_time_only() {
        let mut date_field = make_field("reminder", FieldType::Date);
        date_field.picker_appearance = Some(crate::core::PickerAppearance::TimeOnly);
        let fields = vec![date_field];
        let mut values = HashMap::new();
        values.insert("reminder".to_string(), "14:30".to_string());
        let result = build_value_contexts(&fields, &values, &HashMap::new(), false, false);
        assert_eq!(result[0]["picker_appearance"], "timeOnly");
        assert_eq!(result[0]["value"], "14:30");
    }

    #[test]
    fn build_field_contexts_date_month_only() {
        let mut date_field = make_field("birth_month", FieldType::Date);
        date_field.picker_appearance = Some(crate::core::PickerAppearance::MonthOnly);
        let fields = vec![date_field];
        let mut values = HashMap::new();
        values.insert("birth_month".to_string(), "2026-01".to_string());
        let result = build_value_contexts(&fields, &values, &HashMap::new(), false, false);
        assert_eq!(result[0]["picker_appearance"], "monthOnly");
        assert_eq!(result[0]["value"], "2026-01");
    }

    #[test]
    fn build_field_contexts_date_short_value_day_only() {
        let mut values = HashMap::new();
        values.insert("d".to_string(), "short".to_string()); // less than 10 chars
        let field = make_field("d", FieldType::Date);
        let fields = vec![field];
        let result = build_value_contexts(&fields, &values, &HashMap::new(), false, false);
        // Should use the short value as-is.
        assert_eq!(result[0]["date_only_value"], "short");
    }

    #[test]
    fn build_field_contexts_date_short_value_day_and_time() {
        let mut field = make_field("d", FieldType::Date);
        field.picker_appearance = Some(crate::core::PickerAppearance::DayAndTime);
        let mut values = HashMap::new();
        values.insert("d".to_string(), "short".to_string()); // less than 16 chars
        let fields = vec![field];
        let result = build_value_contexts(&fields, &values, &HashMap::new(), false, false);
        assert_eq!(result[0]["datetime_local_value"], "short");
    }

    // ── Recursive composites: template_id, indexed names, deep nesting ─

    #[test]
    fn build_field_contexts_array_has_template_id() {
        let mut arr_field = make_field("items", FieldType::Array);
        arr_field.fields = vec![make_field("title", FieldType::Text)];
        let fields = vec![arr_field];
        let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
        assert_eq!(result[0]["template_id"], "items");
    }

    #[test]
    fn build_field_contexts_blocks_has_template_id() {
        let mut blocks_field = make_field("content", FieldType::Blocks);
        blocks_field.blocks = vec![BlockDefinition::new(
            "text",
            vec![make_field("body", FieldType::Text)],
        )];
        let fields = vec![blocks_field];
        let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
        assert_eq!(result[0]["template_id"], "content");
    }

    #[test]
    fn build_field_contexts_array_sub_fields_have_indexed_names() {
        let mut arr_field = make_field("slides", FieldType::Array);
        arr_field.fields = vec![
            make_field("title", FieldType::Text),
            make_field("body", FieldType::Textarea),
        ];
        let fields = vec![arr_field];
        let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
        let sub_fields = result[0]["sub_fields"].as_array().unwrap();
        // Sub-fields in the template context should have __INDEX__ placeholder names.
        assert_eq!(sub_fields[0]["name"], "slides[__INDEX__][title]");
        assert_eq!(sub_fields[1]["name"], "slides[__INDEX__][body]");
    }

    #[test]
    fn build_field_contexts_nested_array_in_blocks() {
        let mut inner_array = make_field("images", FieldType::Array);
        inner_array.fields = vec![
            make_field("url", FieldType::Text),
            make_field("caption", FieldType::Text),
        ];
        let mut blocks_field = make_field("content", FieldType::Blocks);
        blocks_field.blocks = vec![{
            let mut bd = BlockDefinition::new(
                "gallery",
                vec![make_field("title", FieldType::Text), inner_array],
            );
            bd.label = Some(LocalizedString::Plain("Gallery".to_string()));
            bd
        }];
        let fields = vec![blocks_field];
        let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);

        let block_defs = result[0]["block_definitions"].as_array().unwrap();
        assert_eq!(block_defs.len(), 1);
        let block_fields = block_defs[0]["fields"].as_array().unwrap();
        assert_eq!(block_fields.len(), 2);

        assert_eq!(block_fields[0]["field_type"], "text");
        assert_eq!(block_fields[0]["name"], "content[__INDEX__][title]");

        assert_eq!(block_fields[1]["field_type"], "array");
        assert_eq!(block_fields[1]["name"], "content[__INDEX__][images]");

        let nested_sub_fields = block_fields[1]["sub_fields"].as_array().unwrap();
        assert_eq!(nested_sub_fields.len(), 2);
        assert_eq!(
            nested_sub_fields[0]["name"],
            "content[__INDEX__][images][__INDEX__][url]"
        );
        assert_eq!(
            nested_sub_fields[1]["name"],
            "content[__INDEX__][images][__INDEX__][caption]"
        );

        assert!(block_fields[1]["template_id"].as_str().is_some());
    }

    #[test]
    fn build_field_contexts_nested_blocks_in_array() {
        let mut inner_blocks = make_field("sections", FieldType::Blocks);
        inner_blocks.blocks = vec![BlockDefinition::new(
            "text",
            vec![make_field("body", FieldType::Richtext)],
        )];
        let mut arr_field = make_field("pages", FieldType::Array);
        arr_field.fields = vec![make_field("title", FieldType::Text), inner_blocks];
        let fields = vec![arr_field];
        let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);

        let sub_fields = result[0]["sub_fields"].as_array().unwrap();
        assert_eq!(sub_fields.len(), 2);
        assert_eq!(sub_fields[0]["field_type"], "text");
        assert_eq!(sub_fields[1]["field_type"], "blocks");

        let nested_block_defs = sub_fields[1]["block_definitions"].as_array().unwrap();
        assert_eq!(nested_block_defs.len(), 1);
        assert_eq!(nested_block_defs[0]["block_type"], "text");

        let nested_block_fields = nested_block_defs[0]["fields"].as_array().unwrap();
        assert_eq!(nested_block_fields[0]["field_type"], "richtext");
        assert_eq!(
            nested_block_fields[0]["name"],
            "pages[__INDEX__][sections][__INDEX__][body]"
        );
    }

    #[test]
    fn build_field_contexts_nested_group_in_array() {
        let mut inner_group = make_field("meta", FieldType::Group);
        inner_group.fields = vec![
            make_field("author", FieldType::Text),
            make_field("date", FieldType::Date),
        ];
        let mut arr_field = make_field("entries", FieldType::Array);
        arr_field.fields = vec![inner_group];
        let fields = vec![arr_field];
        let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);

        let sub_fields = result[0]["sub_fields"].as_array().unwrap();
        assert_eq!(sub_fields.len(), 1);
        assert_eq!(sub_fields[0]["field_type"], "group");

        let group_sub_fields = sub_fields[0]["sub_fields"].as_array().unwrap();
        assert_eq!(group_sub_fields.len(), 2);
        // A group nested in an array row indexes its children as
        // `<group>[0][field]` — the `[0]` the form parser requires to recognize
        // the group as a single object in a new row (see `construct_group` and
        // `single.rs::group_in_array_row_indexes_children_with_zero`). Without it
        // a newly-added row's group data is dropped on save.
        assert_eq!(
            group_sub_fields[0]["name"],
            "entries[__INDEX__][meta][0][author]"
        );
        assert_eq!(
            group_sub_fields[1]["name"],
            "entries[__INDEX__][meta][0][date]"
        );
    }

    #[test]
    fn build_field_contexts_nested_array_in_array() {
        let mut inner_array = make_field("tags", FieldType::Array);
        inner_array.fields = vec![make_field("name", FieldType::Text)];
        let mut outer_array = make_field("items", FieldType::Array);
        outer_array.fields = vec![make_field("title", FieldType::Text), inner_array];
        let fields = vec![outer_array];
        let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);

        let sub_fields = result[0]["sub_fields"].as_array().unwrap();
        assert_eq!(sub_fields[1]["field_type"], "array");

        let nested_sub = sub_fields[1]["sub_fields"].as_array().unwrap();
        assert_eq!(
            nested_sub[0]["name"],
            "items[__INDEX__][tags][__INDEX__][name]"
        );
    }

    // ── filter_hidden ─────────────────────────────────────────────────

    #[test]
    fn build_field_contexts_filter_hidden_removes_hidden_fields() {
        let mut hidden_field = make_field("secret", FieldType::Text);
        hidden_field.admin.hidden = true;
        let fields = vec![
            make_field("title", FieldType::Text),
            hidden_field,
            make_field("body", FieldType::Textarea),
        ];
        let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), true, false);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0]["name"], "title");
        assert_eq!(result[1]["name"], "body");
    }

    #[test]
    fn build_field_contexts_no_filter_includes_hidden_fields() {
        let mut hidden_field = make_field("secret", FieldType::Text);
        hidden_field.admin.hidden = true;
        let fields = vec![make_field("title", FieldType::Text), hidden_field];
        let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
        assert_eq!(result.len(), 2);
    }

    /// Top-level `hidden = true` is unconditional: even with `filter_hidden = false`
    /// the field is skipped, because its data is stripped from API responses.
    #[test]
    fn build_field_contexts_top_level_hidden_always_skipped() {
        let mut api_hidden_field = make_field("internal", FieldType::Text);
        api_hidden_field.hidden = true;

        let fields = vec![make_field("title", FieldType::Text), api_hidden_field];

        let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["name"], "title");
    }

    /// Regression (zip-alignment): `visible_field_defs` must yield the SAME
    /// field sequence `build_field_contexts` builds — `apply_display_conditions`
    /// and `enrich_field_contexts` zip their per-field results against it, so a
    /// divergence silently applies the wrong field's condition/enrichment. A
    /// top-level `hidden` field before later fields used to desync them (defs
    /// kept the hidden field; the built contexts dropped it).
    #[test]
    fn visible_field_defs_matches_built_contexts_with_hidden_fields() {
        let mut internal = make_field("internal", FieldType::Text);
        internal.hidden = true; // always dropped
        let mut admin_only = make_field("admin_only", FieldType::Text);
        admin_only.admin.hidden = true; // dropped only when filter_hidden
        let fields = vec![
            internal,
            make_field("title", FieldType::Text),
            admin_only,
            make_field("body", FieldType::Text),
        ];

        for filter_hidden in [true, false] {
            let built = build_value_contexts(
                &fields,
                &HashMap::new(),
                &HashMap::new(),
                filter_hidden,
                false,
            );
            let built_names: Vec<&str> =
                built.iter().map(|c| c["name"].as_str().unwrap()).collect();
            let def_names: Vec<&str> = visible_field_defs(&fields, filter_hidden)
                .map(|f| f.name.as_str())
                .collect();
            assert_eq!(built_names, def_names, "filter_hidden={filter_hidden}");
        }
    }

    /// `admin.hidden = true` (only) is the upload-meta case: the data must NOT
    /// be stripped from API responses, only from the admin form when
    /// `filter_hidden = true`.
    #[test]
    fn build_field_contexts_admin_hidden_does_not_imply_top_level_hidden() {
        let mut admin_hidden = make_field("url", FieldType::Text);
        admin_hidden.admin.hidden = true;
        assert!(!admin_hidden.hidden);

        let fields = vec![make_field("title", FieldType::Text), admin_hidden];

        let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
        assert_eq!(result.len(), 2);
    }

    // ── Relationship ──────────────────────────────────────────────────

    #[test]
    fn build_field_contexts_relationship_has_collection_info() {
        let mut rel_field = make_field("author", FieldType::Relationship);
        rel_field.relationship = Some(RelationshipConfig::new("users", false));
        let fields = vec![rel_field];
        let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
        assert_eq!(result[0]["relationship_collection"], "users");
        assert_eq!(result[0]["has_many"], false);
    }

    #[test]
    fn build_field_contexts_relationship_has_many() {
        let mut rel_field = make_field("tags", FieldType::Relationship);
        rel_field.relationship = Some(RelationshipConfig::new("tags", true));
        let fields = vec![rel_field];
        let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
        assert_eq!(result[0]["relationship_collection"], "tags");
        assert_eq!(result[0]["has_many"], true);
    }

    // ── Checkbox ──────────────────────────────────────────────────────

    #[test]
    fn build_field_contexts_checkbox_checked_values() {
        for val in &["1", "true", "on", "yes"] {
            let mut values = HashMap::new();
            values.insert("active".to_string(), val.to_string());
            let fields = vec![make_field("active", FieldType::Checkbox)];
            let result = build_value_contexts(&fields, &values, &HashMap::new(), false, false);
            assert_eq!(
                result[0]["checked"], true,
                "Checkbox should be checked for value '{val}'"
            );
        }
    }

    #[test]
    fn build_field_contexts_checkbox_unchecked_values() {
        for val in &["0", "false", "off", "no", ""] {
            let mut values = HashMap::new();
            values.insert("active".to_string(), val.to_string());
            let fields = vec![make_field("active", FieldType::Checkbox)];
            let result = build_value_contexts(&fields, &values, &HashMap::new(), false, false);
            assert_eq!(
                result[0]["checked"], false,
                "Checkbox should be unchecked for value '{val}'"
            );
        }
    }

    // ── Upload ────────────────────────────────────────────────────────

    #[test]
    fn build_field_contexts_upload_has_collection() {
        let mut upload_field = make_field("image", FieldType::Upload);
        upload_field.relationship = Some(RelationshipConfig::new("media", false));
        let fields = vec![upload_field];
        let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
        assert_eq!(result[0]["relationship_collection"], "media");
        assert_eq!(
            result[0]["picker"], "drawer",
            "upload fields default to drawer picker"
        );
    }

    // ── Select ────────────────────────────────────────────────────────

    #[test]
    fn build_field_contexts_select_marks_selected_option() {
        let mut sel = make_field("color", FieldType::Select);
        sel.options = vec![
            crate::core::SelectOption::new(LocalizedString::Plain("Red".to_string()), "red"),
            crate::core::SelectOption::new(LocalizedString::Plain("Blue".to_string()), "blue"),
        ];
        let mut values = HashMap::new();
        values.insert("color".to_string(), "blue".to_string());
        let fields = vec![sel];
        let result = build_value_contexts(&fields, &values, &HashMap::new(), false, false);
        let opts = result[0]["options"].as_array().unwrap();
        assert_eq!(opts[0]["selected"], false);
        assert_eq!(opts[1]["selected"], true);
    }

    // ── Error propagation ─────────────────────────────────────────────

    #[test]
    fn build_field_contexts_errors_attached_to_fields() {
        let fields = vec![make_field("title", FieldType::Text)];
        let mut errors = HashMap::new();
        errors.insert("title".to_string(), "Title is required".to_string());
        let result = build_value_contexts(&fields, &HashMap::new(), &errors, false, false);
        assert_eq!(result[0]["error"], "Title is required");
    }

    #[test]
    fn build_field_contexts_no_error_when_field_valid() {
        let fields = vec![make_field("title", FieldType::Text)];
        let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
        assert!(result[0].get("error").is_none());
    }

    // ── Locale locking ────────────────────────────────────────────────

    #[test]
    fn build_field_contexts_locale_locked_non_localized_field() {
        let fields = vec![make_field("slug", FieldType::Text)];
        let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, true);
        assert_eq!(result[0]["locale_locked"], true);
        assert_eq!(result[0]["readonly"], true);
    }

    #[test]
    fn build_field_contexts_localized_field_not_locked() {
        let mut field = make_field("title", FieldType::Text);
        field.localized = true;
        let fields = vec![field];
        let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, true);
        assert_eq!(result[0]["locale_locked"], false);
        assert_eq!(result[0]["readonly"], false);
    }

    // ── Group ─────────────────────────────────────────────────────────

    #[test]
    fn build_field_contexts_top_level_group_uses_double_underscore() {
        let mut group = make_field("seo", FieldType::Group);
        group.fields = vec![
            make_field("title", FieldType::Text),
            make_field("description", FieldType::Textarea),
        ];
        let fields = vec![group];
        let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
        let sub_fields = result[0]["sub_fields"].as_array().unwrap();
        assert_eq!(sub_fields[0]["name"], "seo__title");
        assert_eq!(sub_fields[1]["name"], "seo__description");
    }

    #[test]
    fn build_field_contexts_group_collapsed() {
        let mut group = make_field("meta", FieldType::Group);
        group.admin.collapsed = true;
        group.fields = vec![make_field("author", FieldType::Text)];
        let fields = vec![group];
        let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
        assert_eq!(result[0]["collapsed"], true);
    }

    #[test]
    fn build_field_contexts_group_sub_field_values() {
        let mut group = make_field("seo", FieldType::Group);
        group.fields = vec![make_field("title", FieldType::Text)];
        let mut values = HashMap::new();
        values.insert("seo__title".to_string(), "My SEO Title".to_string());
        let fields = vec![group];
        let result = build_value_contexts(&fields, &values, &HashMap::new(), false, false);
        let sub_fields = result[0]["sub_fields"].as_array().unwrap();
        assert_eq!(sub_fields[0]["value"], "My SEO Title");
    }

    // ── Array admin options ───────────────────────────────────────────

    #[test]
    fn build_field_contexts_array_with_min_max_rows() {
        let mut arr = make_field("items", FieldType::Array);
        arr.fields = vec![make_field("title", FieldType::Text)];
        arr.min_rows = Some(1);
        arr.max_rows = Some(5);
        let fields = vec![arr];
        let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
        assert_eq!(result[0]["min_rows"], 1);
        assert_eq!(result[0]["max_rows"], 5);
    }

    #[test]
    fn build_field_contexts_array_collapsed() {
        let mut arr = make_field("items", FieldType::Array);
        arr.fields = vec![make_field("title", FieldType::Text)];
        // collapsed defaults to true.
        let fields = vec![arr];
        let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
        assert_eq!(result[0]["init_collapsed"], true);

        // opt-out: collapsed = false.
        let mut arr2 = make_field("items", FieldType::Array);
        arr2.fields = vec![make_field("title", FieldType::Text)];
        arr2.admin.collapsed = false;
        let fields2 = vec![arr2];
        let result2 =
            build_value_contexts(&fields2, &HashMap::new(), &HashMap::new(), false, false);
        assert_eq!(result2[0]["init_collapsed"], false);
    }

    #[test]
    fn build_field_contexts_array_labels_singular() {
        let mut arr = make_field("slides", FieldType::Array);
        arr.fields = vec![make_field("title", FieldType::Text)];
        arr.admin.labels.singular = Some(LocalizedString::Plain("Slide".to_string()));
        let fields = vec![arr];
        let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
        assert_eq!(result[0]["add_label"], "Slide");
    }

    #[test]
    fn build_field_contexts_array_label_field() {
        let mut arr = make_field("items", FieldType::Array);
        arr.fields = vec![make_field("title", FieldType::Text)];
        arr.admin.label_field = Some("title".to_string());
        let fields = vec![arr];
        let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
        assert_eq!(result[0]["label_field"], "title");
    }

    // ── Blocks admin options ──────────────────────────────────────────

    #[test]
    fn build_field_contexts_blocks_with_min_max_rows() {
        let mut blocks = make_field("content", FieldType::Blocks);
        blocks.blocks = vec![BlockDefinition::new(
            "text",
            vec![make_field("body", FieldType::Text)],
        )];
        blocks.min_rows = Some(1);
        blocks.max_rows = Some(10);
        let fields = vec![blocks];
        let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
        assert_eq!(result[0]["min_rows"], 1);
        assert_eq!(result[0]["max_rows"], 10);
    }

    #[test]
    fn build_field_contexts_blocks_collapsed() {
        let mut blocks = make_field("content", FieldType::Blocks);
        blocks.blocks = vec![BlockDefinition::new(
            "text",
            vec![make_field("body", FieldType::Text)],
        )];
        let fields = vec![blocks];
        let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
        assert_eq!(result[0]["init_collapsed"], true);
    }

    #[test]
    fn build_field_contexts_blocks_labels_singular() {
        let mut blocks = make_field("content", FieldType::Blocks);
        blocks.blocks = vec![BlockDefinition::new(
            "text",
            vec![make_field("body", FieldType::Text)],
        )];
        blocks.admin.labels.singular = Some(LocalizedString::Plain("Block".to_string()));
        let fields = vec![blocks];
        let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
        assert_eq!(result[0]["add_label"], "Block");
    }

    #[test]
    fn build_field_contexts_blocks_block_label_field() {
        let mut blocks = make_field("content", FieldType::Blocks);
        blocks.blocks = vec![{
            let mut bd = BlockDefinition::new("text", vec![make_field("body", FieldType::Text)]);
            bd.label_field = Some("body".to_string());
            bd
        }];
        let fields = vec![blocks];
        let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
        let block_defs = result[0]["block_definitions"].as_array().unwrap();
        assert_eq!(block_defs[0]["label_field"], "body");
    }

    #[test]
    fn build_field_contexts_blocks_group_and_image_url() {
        let mut blocks = make_field("content", FieldType::Blocks);
        blocks.blocks = vec![
            {
                let mut bd = BlockDefinition::new("hero", vec![]);
                bd.label = Some(LocalizedString::Plain("Hero".to_string()));
                bd.group = Some("Layout".to_string());
                bd.image_url = Some("/static/blocks/hero.svg".to_string());
                bd
            },
            {
                let mut bd = BlockDefinition::new("text", vec![]);
                bd.label = Some(LocalizedString::Plain("Text".to_string()));
                bd.group = Some("Content".to_string());
                bd
            },
            {
                let mut bd = BlockDefinition::new("divider", vec![]);
                bd.label = Some(LocalizedString::Plain("Divider".to_string()));
                bd
            },
        ];
        let fields = vec![blocks];
        let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
        let block_defs = result[0]["block_definitions"].as_array().unwrap();

        assert_eq!(block_defs[0]["group"], "Layout");
        assert_eq!(block_defs[0]["image_url"], "/static/blocks/hero.svg");

        assert_eq!(block_defs[1]["group"], "Content");
        assert!(
            block_defs[1]
                .get("image_url")
                .is_none_or(serde_json::Value::is_null)
        );

        assert!(
            block_defs[2]
                .get("group")
                .is_none_or(serde_json::Value::is_null)
        );
        assert!(
            block_defs[2]
                .get("image_url")
                .is_none_or(serde_json::Value::is_null)
        );
    }

    #[test]
    fn build_field_contexts_blocks_picker_card() {
        let mut blocks = make_field("content", FieldType::Blocks);
        blocks.admin.picker = Some("card".to_string());
        blocks.blocks = vec![BlockDefinition::new(
            "text",
            vec![make_field("body", FieldType::Text)],
        )];
        let fields = vec![blocks];
        let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
        assert_eq!(result[0]["picker"], "card");
    }

    // ── has_many regression in composites ─────────────────────────────

    /// Bug fix: `has_many` text inside a Group should produce `tags` /
    /// `has_many` context.
    #[test]
    fn has_many_text_in_group_gets_tags_context() {
        let mut group = make_field("meta", FieldType::Group);
        let mut tags = make_field("tags", FieldType::Text);
        tags.has_many = true;
        group.fields = vec![tags];
        let fields = vec![group];

        let mut values = HashMap::new();
        values.insert("meta__tags".to_string(), r#"["rust","lua"]"#.to_string());
        let result = build_value_contexts(&fields, &values, &HashMap::new(), false, false);

        let sub = result[0]["sub_fields"].as_array().unwrap();
        assert_eq!(sub[0]["has_many"], true);
        let tags_arr = sub[0]["tags"].as_array().unwrap();
        assert_eq!(tags_arr.len(), 2);
        assert_eq!(tags_arr[0], "rust");
        assert_eq!(tags_arr[1], "lua");
        assert_eq!(sub[0]["value"], "rust,lua");
    }

    // ── Position / labels / readonly ──────────────────────────────────

    #[test]
    fn build_field_contexts_position_set() {
        let mut field = make_field("status", FieldType::Text);
        field.admin.position = Some("sidebar".to_string());
        let fields = vec![field];
        let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
        assert_eq!(result[0]["position"], "sidebar");
    }

    #[test]
    fn build_field_contexts_custom_label_placeholder_description() {
        let mut field = make_field("title", FieldType::Text);
        field.admin.label = Some(LocalizedString::Plain("Custom Title".to_string()));
        field.admin.placeholder = Some(LocalizedString::Plain("Enter title here...".to_string()));
        field.admin.description = Some(LocalizedString::Plain("The main title".to_string()));
        let fields = vec![field];
        let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
        assert_eq!(result[0]["label"], "Custom Title");
        assert_eq!(result[0]["placeholder"], "Enter title here...");
        assert_eq!(result[0]["description"], "The main title");
    }

    #[test]
    fn build_field_contexts_readonly_field() {
        let mut field = make_field("slug", FieldType::Text);
        field.admin.readonly = true;
        let fields = vec![field];
        let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
        assert_eq!(result[0]["readonly"], true);
    }

    // ── Tabs error_count ──────────────────────────────────────────────

    #[test]
    fn tabs_field_context_includes_error_count() {
        let mut tabs_field = make_field("settings", FieldType::Tabs);
        tabs_field.tabs = vec![
            FieldTab::new(
                "General",
                vec![
                    {
                        let mut f = make_field("title", FieldType::Text);
                        f.required = true;
                        f
                    },
                    make_field("slug", FieldType::Text),
                ],
            ),
            FieldTab::new("Advanced", vec![make_field("meta", FieldType::Text)]),
        ];

        let values = HashMap::new();
        let mut errors = HashMap::new();
        errors.insert("title".to_string(), "Title is required".to_string());

        let result = build_value_contexts(&[tabs_field], &values, &errors, false, false);
        let tabs = result[0]["tabs"]
            .as_array()
            .expect("tabs should be an array");

        // First tab has 1 error (title is required).
        assert_eq!(tabs[0]["error_count"], 1);
        // Second tab has no errors.
        assert!(tabs[1].get("error_count").is_none() || tabs[1]["error_count"].is_null());
    }

    // ── richtext_format ──────────────────────────────────────────────

    #[test]
    fn richtext_format_defaults_to_html() {
        let field = make_field("body", FieldType::Richtext);
        let fields = vec![field];
        let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
        assert_eq!(result[0]["richtext_format"], "html");
    }

    #[test]
    fn richtext_format_json() {
        let mut field = make_field("body", FieldType::Richtext);
        field.admin.richtext_format = Some("json".to_string());
        let fields = vec![field];
        let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
        assert_eq!(result[0]["richtext_format"], "json");
    }

    // ── Recursion guard ──────────────────────────────────────────────

    #[test]
    fn max_depth_prevents_infinite_recursion() {
        fn make_nested_array(depth: usize) -> crate::core::FieldDefinition {
            let mut field =
                crate::core::FieldDefinition::builder(format!("level{depth}"), FieldType::Array)
                    .build();
            if depth < 10 {
                field.fields = vec![make_nested_array(depth + 1)];
            } else {
                field.fields =
                    vec![crate::core::FieldDefinition::builder("leaf", FieldType::Text).build()];
            }
            field
        }
        let deep = make_nested_array(0);
        let fields = vec![deep];
        // Must not stack overflow — MAX_FIELD_DEPTH caps recursion.
        let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["field_type"], "array");
    }

    // ── Richtext node attr error display ──────────────────────────────

    #[test]
    fn build_richtext_field_shows_node_attr_errors() {
        let field = make_field("content", FieldType::Richtext);
        let fields = vec![field];
        let values = HashMap::new();
        let mut errors = HashMap::new();
        errors.insert(
            "content[cta#0].text".to_string(),
            "Text is required".to_string(),
        );

        let result = build_value_contexts(&fields, &values, &errors, false, false);
        assert_eq!(result[0]["field_type"], "richtext");
        assert_eq!(result[0]["error"], "Text is required");
    }

    #[test]
    fn build_richtext_field_direct_error_takes_priority() {
        let field = make_field("content", FieldType::Richtext);
        let fields = vec![field];
        let values = HashMap::new();
        let mut errors = HashMap::new();
        errors.insert("content".to_string(), "Field is required".to_string());
        errors.insert(
            "content[cta#0].text".to_string(),
            "Text is required".to_string(),
        );

        let result = build_value_contexts(&fields, &values, &errors, false, false);
        assert_eq!(result[0]["error"], "Field is required");
    }

    #[test]
    fn build_richtext_field_no_errors_no_error_key() {
        let field = make_field("content", FieldType::Richtext);
        let fields = vec![field];
        let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
        assert!(result[0].get("error").is_none() || result[0]["error"].is_null());
    }
}
