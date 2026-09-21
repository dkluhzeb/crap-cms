//! Builds enriched typed child field contexts for layout wrappers (Row,
//! Collapsible, Tabs) inside Array and Blocks rows.
//!
//! Called from `sub_row_collapsible_row`, `sub_row_collapsible_group`, and
//! `build_tab_context` in [`field_types`](super::field_types) when the layout
//! wrapper sits inside an Array/Blocks row. The naming convention mirrors
//! enrichment-phase semantics:
//!
//! - Layout wrappers (Row/Collapsible/Tabs) are transparent — their children
//!   inherit the parent's bracketed name (e.g. `items[0][title]`, not
//!   `items[0][row][title]`).
//! - Group children get a `[0]` suffix in their parent prefix
//!   (e.g. `items[0][meta][0][title]`).
//! - Array/Blocks inside layout wrappers render template-only (no row
//!   iteration). Their data isn't recursed; only the new-row template
//!   sub-fields are populated.

use std::collections::HashMap;

use serde_json::Value;

use crate::{
    admin::{
        context::field::{
            ArrayField, BaseFieldData, BlockDefinition, BlocksField, CodeField, ConditionData,
            FieldContext, TabPanel, TextareaField, ValidationAttrs,
        },
        handlers::{
            field_context::{
                MAX_FIELD_DEPTH,
                builder::build_single_field_context,
                cascaded_readonly, count_errors_in_field_contexts,
                enrich::{field_types, nested::construct_sub_variant, nested::enrich_sub_richtext},
                locale_locked_display, localize_date_display, readonly_display, safe_template_id,
            },
            shared::admin_form_fields,
        },
    },
    core::{
        Builder,
        field::{FieldDefinition, FieldType},
    },
    db::query::helpers::{lang_column, tz_column},
};

/// Inheritance state passed down through recursion in this module.
#[derive(Builder)]
pub struct ChildEnrichOpts<'a> {
    pub locale_locked: bool,
    pub non_default_locale: bool,
    /// Whether a container around these children declares `admin.readonly`.
    /// It cascades downward, so every child built with this set renders
    /// read-only whatever its own `admin.readonly` says. A container locked
    /// only by the locale does not set it — the locale lock is recomputed per
    /// child from `non_default_locale`.
    pub ancestor_readonly: bool,
    pub depth: usize,
    #[builder(required)]
    pub errors: &'a HashMap<String, String>,
}

/// The same inheritance state with `ancestor_readonly` and `depth` replaced —
/// what a container hands to the fields inside it.
fn inherited_opts<'a>(
    opts: &ChildEnrichOpts<'a>,
    ancestor_readonly: bool,
    depth: usize,
) -> ChildEnrichOpts<'a> {
    ChildEnrichOpts::builder(opts.errors)
        .locale_locked(opts.locale_locked)
        .non_default_locale(opts.non_default_locale)
        .ancestor_readonly(ancestor_readonly)
        .depth(depth)
        .build()
}

/// Resolve the child's form name and raw JSON value.
///
/// Layout wrappers are transparent — they inherit the parent name and the full
/// data object. Leaf fields get `parent_name[field_name]` and their own value.
fn resolve_child_name_and_value<'a>(
    child: &FieldDefinition,
    data: Option<&'a Value>,
    data_obj: Option<&'a serde_json::Map<String, Value>>,
    parent_name: &str,
) -> (String, Option<&'a Value>, String) {
    let is_wrapper = matches!(
        child.field_type,
        FieldType::Tabs | FieldType::Row | FieldType::Collapsible
    );

    let child_raw = if is_wrapper {
        data
    } else {
        data_obj.and_then(|m| m.get(&child.name))
    };

    let child_name = if is_wrapper {
        parent_name.to_string()
    } else {
        format!("{}[{}]", parent_name, child.name)
    };

    let child_val = child_raw
        .map(|v| match v {
            Value::String(s) => s.clone(),
            Value::Null => String::new(),
            _ if is_wrapper => String::new(),
            other => other.to_string(),
        })
        .unwrap_or_default();

    (child_name, child_raw, child_val)
}

/// Build the typed shared base data for a child field. `locale_locked` is
/// recomputed per child as `non_default_locale && !child.localized`, matching
/// the build-phase semantics. A localized field inside a non-localized layout
/// wrapper must stay editable in non-default locales. `readonly` is the
/// broader flag: it also carries down from a read-only container.
fn build_child_base(
    child: &FieldDefinition,
    child_name: &str,
    child_val: &str,
    opts: &ChildEnrichOpts,
) -> BaseFieldData {
    let label = child.resolved_label();

    let locale_locked = locale_locked_display(opts.non_default_locale, child);

    BaseFieldData {
        name: child_name.to_string(),
        field_name: child.name.clone(),
        label,
        required: child.required,
        value: Value::String(child_val.to_string()),
        placeholder: child
            .admin
            .placeholder
            .as_ref()
            .map(|ls| ls.resolve_default().to_string()),
        description: child
            .admin
            .description
            .as_ref()
            .map(|ls| ls.resolve_default().to_string()),
        readonly: readonly_display(child, opts.ancestor_readonly, locale_locked),
        localized: child.localized,
        locale_locked,
        position: child.admin.position.clone(),
        template: child.admin.template.clone(),
        extra: child.admin.extra.clone(),
        error: opts.errors.get(child_name).cloned(),
        validation: ValidationAttrs::default(),
        condition: ConditionData::default(),
    }
}

/// Apply Array template-only enrichment (no row iteration). Layout-wrapper
/// children render only the new-row template sub-fields; existing data rows
/// aren't recursed into here.
fn apply_array_template(
    af: &mut ArrayField,
    child: &FieldDefinition,
    child_name: &str,
    opts: &ChildEnrichOpts,
) {
    let template_prefix = format!("{child_name}[__INDEX__]");

    af.sub_fields = admin_form_fields(&child.fields)
        .map(|sf| {
            build_single_field_context(
                sf,
                &HashMap::new(),
                &HashMap::new(),
                &template_prefix,
                opts.non_default_locale,
                opts.ancestor_readonly,
                opts.depth + 1,
            )
        })
        .collect();

    af.row_count = 0;
    af.template_id = safe_template_id(child_name);
    af.min_rows = child.min_rows;
    af.max_rows = child.max_rows;
    af.init_collapsed = child.admin.collapsed;
    af.add_label = child
        .admin
        .labels
        .singular
        .as_ref()
        .map(|ls| ls.resolve_default().to_string());
    af.label_field.clone_from(&child.admin.label_field);
}

/// Apply Blocks template-only enrichment (no row iteration). Mirrors
/// [`apply_array_template`] — only the new-row block-definition templates
/// are populated.
fn apply_blocks_template(
    bf: &mut BlocksField,
    child: &FieldDefinition,
    child_name: &str,
    opts: &ChildEnrichOpts,
) {
    let template_prefix = format!("{child_name}[__INDEX__]");

    bf.block_definitions = child
        .blocks
        .iter()
        .map(|bd| {
            let fields: Vec<FieldContext> = admin_form_fields(&bd.fields)
                .map(|sf| {
                    build_single_field_context(
                        sf,
                        &HashMap::new(),
                        &HashMap::new(),
                        &template_prefix,
                        opts.non_default_locale,
                        opts.ancestor_readonly,
                        opts.depth + 1,
                    )
                })
                .collect();

            let label = bd.display_label();

            BlockDefinition {
                block_type: bd.block_type.clone(),
                label,
                fields,
                label_field: bd.label_field.clone(),
                group: bd.group.clone(),
                image_url: bd.image_url.clone(),
            }
        })
        .collect();

    bf.row_count = 0;
    bf.template_id = safe_template_id(child_name);
    bf.min_rows = child.min_rows;
    bf.max_rows = child.max_rows;
    bf.init_collapsed = child.admin.collapsed;
    bf.add_label = child
        .admin
        .labels
        .singular
        .as_ref()
        .map(|ls| ls.resolve_default().to_string());
    bf.picker.clone_from(&child.admin.picker);
}

/// Apply Date enrichment with structured-row timezone lookup.
///
/// Builds the date context from the stored value, then takes the zone from the
/// parent row's `<short_name>_tz` companion key and shows the stored UTC value
/// as local time in it (see [`localize_date_display`]).
fn apply_date(
    df: &mut crate::admin::context::field::DateField,
    child: &FieldDefinition,
    child_val: &str,
    data_obj: Option<&serde_json::Map<String, Value>>,
) {
    field_types::sub_date(df, child, child_val, "");

    if !child.has_tz_companion() {
        return;
    }

    let tz_val = data_obj
        .and_then(|m| m.get(&tz_column(&child.name)))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if !tz_val.is_empty() {
        df.timezone_value = Some(tz_val.to_string());
        localize_date_display(df, child_val, tz_val);
    }
}

/// Apply type-specific enrichment to the typed child variant. Each arm
/// either calls a per-variant helper or sets a single field on the variant;
/// the match itself stays as the table of contents for `FieldContext`.
fn dispatch_child(
    fc: &mut FieldContext,
    child: &FieldDefinition,
    child_raw: Option<&Value>,
    data_obj: Option<&serde_json::Map<String, Value>>,
    child_name: &str,
    child_val: &str,
    opts: &ChildEnrichOpts,
) {
    if opts.depth + 1 >= MAX_FIELD_DEPTH {
        return;
    }

    match fc {
        FieldContext::Row(rf) => {
            rf.sub_fields = enrich_container_children(child, child_raw, child_name, opts);
        }
        FieldContext::Collapsible(gf) => {
            gf.sub_fields = enrich_container_children(child, child_raw, child_name, opts);
            gf.collapsed = child.admin.collapsed;
        }
        FieldContext::Group(gf) => {
            // Group adds a `[0]` index suffix for its sub-fields' parent
            // prefix — matches the form parser's "Group is a single-element
            // composite" convention.
            let group_prefix = format!("{child_name}[0]");
            gf.sub_fields = enrich_container_children(child, child_raw, &group_prefix, opts);
            gf.collapsed = child.admin.collapsed;
        }
        FieldContext::Tabs(tf) => {
            tf.tabs = enrich_tab_panels(child, child_raw, child_name, opts);
        }
        FieldContext::Array(af) => apply_array_template(af, child, child_name, opts),
        FieldContext::Blocks(bf) => apply_blocks_template(bf, child, child_name, opts),
        FieldContext::Checkbox(cf) => field_types::sub_checkbox(cf, child_val),
        FieldContext::Select(cf) | FieldContext::Radio(cf) => {
            field_types::sub_select_radio(cf, child, child_val);
        }
        FieldContext::Date(df) => apply_date(df, child, child_val, data_obj),
        FieldContext::Relationship(rf) => field_types::sub_relationship(rf, child),
        FieldContext::Upload(uf) => field_types::sub_upload(uf, child),
        FieldContext::Textarea(tf) => enrich_sub_textarea(tf, child),
        FieldContext::Richtext(rf) => enrich_sub_richtext(rf, child, child_name, opts.errors),
        FieldContext::Code(cf) => enrich_sub_code(cf, child, data_obj),
        FieldContext::Text(tf) if child.has_many => {
            field_types::sub_text_has_many_tags(tf, child_val);
        }
        FieldContext::Number(nf) if child.has_many => {
            field_types::sub_number_has_many_tags(nf, child_val);
        }
        _ => {}
    }
}

/// Recurse into the layout-wrapper's `child.fields` to build typed
/// sub-field contexts. Shared by `Row`, `Collapsible`, and `Group` arms —
/// only the parent-prefix string differs.
fn enrich_container_children(
    child: &FieldDefinition,
    child_raw: Option<&Value>,
    parent_prefix: &str,
    opts: &ChildEnrichOpts,
) -> Vec<FieldContext> {
    let deeper = inherited_opts(opts, opts.ancestor_readonly, opts.depth + 1);

    build_enriched_children_from_data(&child.fields, child_raw, parent_prefix, &deeper)
}

/// Build the per-tab [`TabPanel`] list for a `Tabs` field — each tab's
/// `fields` recurse into typed sub-field contexts and the per-tab error
/// count is precomputed for the UI badge.
fn enrich_tab_panels(
    child: &FieldDefinition,
    child_raw: Option<&Value>,
    child_name: &str,
    opts: &ChildEnrichOpts,
) -> Vec<TabPanel> {
    let deeper = inherited_opts(opts, opts.ancestor_readonly, opts.depth + 1);

    child
        .tabs
        .iter()
        .map(|tab| {
            let tab_sub_fields =
                build_enriched_children_from_data(&tab.fields, child_raw, child_name, &deeper);
            let error_count = count_errors_in_field_contexts(&tab_sub_fields);
            TabPanel {
                label: tab.label.clone(),
                sub_fields: tab_sub_fields,
                error_count: if error_count > 0 {
                    Some(error_count)
                } else {
                    None
                },
                description: tab.description.clone(),
            }
        })
        .collect()
}

/// Apply the textarea-specific admin knobs (`rows`, `resizable`) to a
/// layout-wrapper-nested textarea child.
fn enrich_sub_textarea(tf: &mut TextareaField, child: &FieldDefinition) {
    tf.rows = child.admin.rows.unwrap_or(8);
    tf.resizable = child.admin.resizable;
}

/// Apply the code-editor-specific admin knobs (`language`, `languages`) to a
/// layout-wrapper-nested code child.
///
/// The language starts at the operator default and is replaced by the row's
/// stored `<name>_lang` companion when it holds one — the same lookup the
/// top-level and array builders do, so a layout wrapper stays transparent.
fn enrich_sub_code(
    cf: &mut CodeField,
    child: &FieldDefinition,
    data_obj: Option<&serde_json::Map<String, Value>>,
) {
    cf.language = child
        .admin
        .language
        .as_deref()
        .unwrap_or("json")
        .to_string();

    if !child.has_lang_companion() {
        return;
    }

    cf.languages = Some(child.admin.languages.clone());

    if let Some(lang) = data_obj
        .and_then(|m| m.get(&lang_column(&child.name)))
        .and_then(Value::as_str)
        .filter(|lang| !lang.is_empty())
    {
        cf.language = lang.to_string();
    }
}

/// Build the typed enriched [`FieldContext`] for one child field.
fn build_child(
    child: &FieldDefinition,
    data: Option<&Value>,
    data_obj: Option<&serde_json::Map<String, Value>>,
    parent_name: &str,
    opts: &ChildEnrichOpts,
) -> FieldContext {
    let (child_name, child_raw, child_val) =
        resolve_child_name_and_value(child, data, data_obj, parent_name);

    let base = build_child_base(child, &child_name, &child_val, opts);

    // `admin.readonly` cascades: everything nested inside a read-only
    // container renders read-only too. The locale lock is not carried here —
    // it is recomputed per field from `non_default_locale`.
    let inner_opts = inherited_opts(
        opts,
        cascaded_readonly(child, opts.ancestor_readonly),
        opts.depth,
    );

    let mut fc = construct_sub_variant(child, base, &child_name);

    dispatch_child(
        &mut fc,
        child,
        child_raw,
        data_obj,
        &child_name,
        &child_val,
        &inner_opts,
    );

    fc
}

/// Build typed enriched child field contexts from structured JSON data.
///
/// Used by layout wrapper handlers (Tabs/Row/Collapsible) inside Array/Blocks
/// rows to correctly propagate structured data to nested layout wrappers.
///
/// Only the fields the form renders become contexts ([`admin_form_fields`]), so
/// a hidden field gets no input at any depth and the passes that later zip these
/// contexts against their defs pair the same entries.
pub fn build_enriched_children_from_data(
    fields: &[FieldDefinition],
    data: Option<&Value>,
    parent_name: &str,
    opts: &ChildEnrichOpts,
) -> Vec<FieldContext> {
    if opts.depth >= MAX_FIELD_DEPTH {
        return Vec::new();
    }

    let data_obj = data.and_then(|v| v.as_object());

    admin_form_fields(fields)
        .map(|child| build_child(child, data, data_obj, parent_name, opts))
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::core::FieldAdmin;

    /// A code field inside a layout wrapper takes its language from the row's
    /// stored `<name>_lang` companion, the way the top-level and array builders
    /// do — a wrapper is transparent, so it must not fall back to the operator
    /// default and show the row in the wrong language.
    #[test]
    fn a_wrapped_code_child_takes_its_language_from_the_row() {
        let snippet = FieldDefinition::builder("snippet", FieldType::Code)
            .admin(
                FieldAdmin::builder()
                    .language("javascript")
                    .languages(vec!["javascript".to_string(), "python".to_string()])
                    .build(),
            )
            .build();

        let row = json!({ "snippet": "print(1)", "snippet_lang": "python" });

        let errors = HashMap::new();
        let children = build_enriched_children_from_data(
            &[snippet],
            Some(&row),
            "items[0]",
            &ChildEnrichOpts::builder(&errors).build(),
        );

        let FieldContext::Code(cf) = &children[0] else {
            panic!("expected a code context")
        };
        assert_eq!(cf.language, "python");
        assert_eq!(
            cf.languages.as_deref(),
            Some(&["javascript".to_string(), "python".to_string()][..])
        );
    }

    /// Without a stored pick the operator default stands, and the picker's
    /// allow-list is still emitted.
    #[test]
    fn a_wrapped_code_child_keeps_the_default_language_without_a_pick() {
        let snippet = FieldDefinition::builder("snippet", FieldType::Code)
            .admin(
                FieldAdmin::builder()
                    .language("javascript")
                    .languages(vec!["javascript".to_string(), "python".to_string()])
                    .build(),
            )
            .build();

        let row = json!({ "snippet": "print(1)", "snippet_lang": "" });

        let errors = HashMap::new();
        let children = build_enriched_children_from_data(
            &[snippet],
            Some(&row),
            "items[0]",
            &ChildEnrichOpts::builder(&errors).build(),
        );

        let FieldContext::Code(cf) = &children[0] else {
            panic!("expected a code context")
        };
        assert_eq!(cf.language, "javascript");
        assert!(cf.languages.is_some());
    }

    /// Build the children of a Group with `admin.readonly` set as asked.
    fn group_children(readonly: bool) -> Vec<FieldContext> {
        let meta = FieldDefinition::builder("meta", FieldType::Group)
            .fields(vec![
                FieldDefinition::builder("title", FieldType::Text).build(),
            ])
            .admin(FieldAdmin::builder().readonly(readonly).build())
            .build();

        let data = json!({ "meta": { "title": "Hello" } });
        let errors = HashMap::new();

        build_enriched_children_from_data(
            &[meta],
            Some(&data),
            "items[0]",
            &ChildEnrichOpts::builder(&errors).build(),
        )
    }

    /// A read-only Group locks every field inside it — `admin.readonly` on a
    /// container cascades downward — while a plain Group leaves its children
    /// editable. Neither is a locale lock.
    #[test]
    fn a_readonly_group_cascades_readonly_to_its_children() {
        let locked = group_children(true);
        let FieldContext::Group(gf) = &locked[0] else {
            panic!("expected a group context")
        };

        assert!(gf.base.readonly);
        assert!(
            gf.sub_fields[0].base().readonly,
            "a read-only group locks the fields inside it"
        );
        assert!(
            !gf.sub_fields[0].base().locale_locked,
            "read-only is not the same as locale-locked"
        );

        let open = group_children(false);
        let FieldContext::Group(gf) = &open[0] else {
            panic!("expected a group context")
        };

        assert!(
            !gf.sub_fields[0].base().readonly,
            "a plain group leaves its children editable"
        );
    }

    /// A layout wrapper is transparent for naming but still carries its own
    /// `admin.readonly`, so it locks the fields it wraps.
    #[test]
    fn a_readonly_layout_wrapper_locks_the_fields_it_wraps() {
        let row = FieldDefinition::builder("layout", FieldType::Row)
            .fields(vec![
                FieldDefinition::builder("title", FieldType::Text).build(),
            ])
            .admin(FieldAdmin::builder().readonly(true).build())
            .build();

        let data = json!({ "title": "Hello" });
        let errors = HashMap::new();

        let children = build_enriched_children_from_data(
            &[row],
            Some(&data),
            "items[0]",
            &ChildEnrichOpts::builder(&errors).build(),
        );

        let FieldContext::Row(rf) = &children[0] else {
            panic!("expected a row context")
        };

        assert!(rf.base.readonly);
        assert!(
            rf.sub_fields[0].base().readonly,
            "a read-only wrapper locks the fields it wraps"
        );
    }

    /// Resolve a leaf field's (name, raw, value) from a parent data object.
    fn resolve_leaf(name: &str, ft: FieldType, data: &Value, parent: &str) -> (String, String) {
        let field = FieldDefinition::builder(name, ft).build();
        let obj = data.as_object();
        let (child_name, _raw, child_val) =
            resolve_child_name_and_value(&field, Some(data), obj, parent);
        (child_name, child_val)
    }

    #[test]
    fn leaf_field_gets_bracketed_name_and_its_own_value() {
        let data = json!({ "title": "Hello" });
        let (name, val) = resolve_leaf("title", FieldType::Text, &data, "items[0]");
        assert_eq!(name, "items[0][title]");
        assert_eq!(val, "Hello");
    }

    #[test]
    fn leaf_null_value_becomes_empty_string() {
        let data = json!({ "title": null });
        let (_name, val) = resolve_leaf("title", FieldType::Text, &data, "items[0]");
        assert_eq!(val, "");
    }

    #[test]
    fn leaf_non_string_value_is_stringified() {
        let data = json!({ "count": 42 });
        let (_name, val) = resolve_leaf("count", FieldType::Number, &data, "items[0]");
        assert_eq!(val, "42");
    }

    #[test]
    fn leaf_missing_from_data_yields_empty_value() {
        let data = json!({ "other": "x" });
        let (name, val) = resolve_leaf("title", FieldType::Text, &data, "items[0]");
        assert_eq!(name, "items[0][title]");
        assert_eq!(val, "");
    }

    /// Layout wrappers are transparent: they inherit the parent's bracketed
    /// name (no `[row]` segment) and carry the full data object, with an empty
    /// scalar value.
    #[test]
    fn layout_wrapper_inherits_parent_name_and_is_transparent() {
        let field = FieldDefinition::builder("row", FieldType::Row).build();
        let data = json!({ "title": "Hello" });
        let (child_name, raw, child_val) =
            resolve_child_name_and_value(&field, Some(&data), data.as_object(), "items[0]");

        assert_eq!(child_name, "items[0]");
        assert_eq!(child_val, "");
        assert!(raw.is_some_and(serde_json::Value::is_object));
    }
}
