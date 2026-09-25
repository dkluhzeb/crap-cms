//! Per-variant constructors for composite fields (group, row, collapsible,
//! tabs, array, blocks), recursing into their sub-fields up to
//! [`MAX_FIELD_DEPTH`].

use std::collections::HashMap;

use crate::{
    admin::{
        context::field::{
            ArrayField, BaseFieldData, BlockDefinition as BlockDefCtx, BlocksField, FieldContext,
            GroupField, RowField, TabPanel, TabsField,
        },
        handlers::{
            field_context::{
                MAX_FIELD_DEPTH,
                builder::single::entry::{SingleFieldCtx, build_single_field_context},
                count_errors_in_field_contexts, safe_template_id,
            },
            shared::admin_form_fields,
        },
    },
    core::FieldDefinition,
};

/// Build sub-fields for layout wrappers (Row, Collapsible, Tabs).
/// Top-level wrappers use empty prefix, nested ones use the `full_name`.
///
/// Only the fields the form renders become contexts ([`admin_form_fields`]),
/// so the enrichment and display-condition passes pair the same entries.
fn build_layout_sub_fields(fields: &[FieldDefinition], fc: &SingleFieldCtx) -> Vec<FieldContext> {
    let prefix = if fc.name_prefix.is_empty() {
        ""
    } else {
        fc.full_name
    };

    admin_form_fields(fields)
        .map(|sf| {
            build_single_field_context(
                sf,
                fc.values,
                fc.errors,
                prefix,
                fc.non_default_locale,
                fc.cascade_readonly,
                fc.depth + 1,
            )
        })
        .collect()
}

pub(super) fn construct_group(base: BaseFieldData, fc: &SingleFieldCtx) -> FieldContext {
    let sub_fields = if fc.depth >= MAX_FIELD_DEPTH {
        Vec::new()
    } else {
        let prefix = if fc.name_prefix.is_empty() {
            fc.field.name.clone()
        } else if fc.full_name.contains('[') {
            // Group nested in an array/blocks row: index the group as
            // `<group>[0]` so its children become `<group>[0][field]` — mirroring
            // the enrich phase's `group_child_name` and what the form parser
            // requires to recognize the group as a single object. Without the
            // `[0]`, a newly-added row's group data is dropped on save (the
            // parser can't parse the child index and collapses the group to `{}`).
            format!("{}[0]", fc.full_name)
        } else {
            // Group nested in a top-level group chain: keep `__` column naming.
            fc.full_name.to_string()
        };

        let child_non_default_locale = if fc.field.localized {
            false
        } else {
            fc.non_default_locale
        };

        admin_form_fields(&fc.field.fields)
            .map(|sf| {
                build_single_field_context(
                    sf,
                    fc.values,
                    fc.errors,
                    &prefix,
                    child_non_default_locale,
                    fc.cascade_readonly,
                    fc.depth + 1,
                )
            })
            .collect()
    };

    FieldContext::Group(GroupField {
        base,
        sub_fields,
        collapsed: fc.field.admin.collapsed,
    })
}

pub(super) fn construct_row(base: BaseFieldData, fc: &SingleFieldCtx) -> FieldContext {
    let sub_fields = if fc.depth >= MAX_FIELD_DEPTH {
        Vec::new()
    } else {
        build_layout_sub_fields(&fc.field.fields, fc)
    };

    FieldContext::Row(RowField { base, sub_fields })
}

pub(super) fn construct_collapsible(base: BaseFieldData, fc: &SingleFieldCtx) -> FieldContext {
    let sub_fields = if fc.depth >= MAX_FIELD_DEPTH {
        Vec::new()
    } else {
        build_layout_sub_fields(&fc.field.fields, fc)
    };

    FieldContext::Collapsible(GroupField {
        base,
        sub_fields,
        collapsed: fc.field.admin.collapsed,
    })
}

pub(super) fn construct_tabs(base: BaseFieldData, fc: &SingleFieldCtx) -> FieldContext {
    let tabs: Vec<TabPanel> = if fc.depth >= MAX_FIELD_DEPTH {
        Vec::new()
    } else {
        fc.field
            .tabs
            .iter()
            .map(|tab| {
                let sub_fields = build_layout_sub_fields(&tab.fields, fc);

                let error_count = count_errors_in_field_contexts(&sub_fields);
                let error_count_opt = if error_count > 0 {
                    Some(error_count)
                } else {
                    None
                };

                TabPanel {
                    label: tab.label.clone(),
                    sub_fields,
                    error_count: error_count_opt,
                    description: tab.description.clone(),
                }
            })
            .collect()
    };

    FieldContext::Tabs(TabsField { base, tabs })
}

pub(super) fn construct_array(base: BaseFieldData, fc: &SingleFieldCtx) -> FieldContext {
    let template_prefix = format!("{}[__INDEX__]", fc.full_name);
    let sub_fields: Vec<FieldContext> = if fc.depth >= MAX_FIELD_DEPTH {
        Vec::new()
    } else {
        admin_form_fields(&fc.field.fields)
            .map(|sf| {
                build_single_field_context(
                    sf,
                    &HashMap::new(),
                    &HashMap::new(),
                    &template_prefix,
                    fc.non_default_locale,
                    fc.cascade_readonly,
                    fc.depth + 1,
                )
            })
            .collect()
    };

    FieldContext::Array(ArrayField {
        base,
        sub_fields,
        rows: None,
        row_count: 0,
        template_id: safe_template_id(fc.full_name),
        min_rows: fc.field.min_rows,
        max_rows: fc.field.max_rows,
        init_collapsed: fc.field.admin.collapsed,
        add_label: fc
            .field
            .admin
            .labels
            .singular
            .as_ref()
            .map(|ls| ls.resolve_current().to_string()),
        label_field: fc.field.admin.label_field.clone(),
    })
}

pub(super) fn construct_blocks(base: BaseFieldData, fc: &SingleFieldCtx) -> FieldContext {
    let template_prefix = format!("{}[__INDEX__]", fc.full_name);

    let block_definitions: Vec<BlockDefCtx> = if fc.depth >= MAX_FIELD_DEPTH {
        Vec::new()
    } else {
        fc.field
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
                            fc.non_default_locale,
                            fc.cascade_readonly,
                            fc.depth + 1,
                        )
                    })
                    .collect();

                let label = bd.display_label();

                BlockDefCtx {
                    block_type: bd.block_type.clone(),
                    label,
                    fields,
                    label_field: bd.label_field.clone(),
                    group: bd.group.clone(),
                    image_url: bd.image_url.clone(),
                }
            })
            .collect()
    };

    FieldContext::Blocks(BlocksField {
        base,
        block_definitions,
        rows: None,
        row_count: 0,
        template_id: safe_template_id(fc.full_name),
        min_rows: fc.field.min_rows,
        max_rows: fc.field.max_rows,
        init_collapsed: fc.field.admin.collapsed,
        add_label: fc
            .field
            .admin
            .labels
            .singular
            .as_ref()
            .map(|ls| ls.resolve_current().to_string()),
        picker: fc.field.admin.picker.clone(),
        label_field: fc.field.admin.label_field.clone(),
    })
}

#[cfg(test)]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::case_sensitive_file_extension_comparisons,
    clippy::items_after_statements,
    clippy::match_wildcard_for_single_variants,
    clippy::missing_panics_doc,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::unreadable_literal,
    clippy::used_underscore_binding
)]
mod tests {
    use std::collections::HashMap;

    use crate::admin::handlers::field_context::builder::build_single_field_context;
    use crate::core::{FieldAdmin, FieldDefinition, FieldType};

    fn group_field(name: &str, localized: bool, children: Vec<FieldDefinition>) -> FieldDefinition {
        FieldDefinition {
            name: name.to_string(),
            field_type: FieldType::Group,
            localized,
            fields: children,
            ..Default::default()
        }
    }

    fn text_field(name: &str) -> FieldDefinition {
        FieldDefinition {
            name: name.to_string(),
            field_type: FieldType::Text,
            ..Default::default()
        }
    }

    /// Build a container of `field_type` holding one text sub-field, with
    /// `admin.readonly` set as asked.
    fn readonly_container(field_type: FieldType, readonly: bool) -> FieldDefinition {
        FieldDefinition::builder("meta", field_type)
            .fields(vec![text_field("title")])
            .admin(FieldAdmin::builder().readonly(readonly).build())
            .build()
    }

    /// A read-only container locks what it contains: `admin.readonly` on a
    /// Group cascades to every sub-field. A plain Group leaves its children
    /// editable, and neither case is a locale lock.
    #[test]
    fn a_readonly_group_cascades_readonly_to_its_children() {
        let empty = HashMap::new();

        let locked = readonly_container(FieldType::Group, true);
        let ctx =
            build_single_field_context(&locked, &empty, &empty, "", false, false, 0).to_value();

        assert_eq!(ctx["readonly"], true);
        assert_eq!(
            ctx["sub_fields"][0]["readonly"], true,
            "a read-only group locks the fields inside it"
        );
        assert_eq!(
            ctx["sub_fields"][0]["locale_locked"], false,
            "read-only is not the same as locale-locked"
        );

        let open = readonly_container(FieldType::Group, false);
        let ctx = build_single_field_context(&open, &empty, &empty, "", false, false, 0).to_value();

        assert_eq!(
            ctx["sub_fields"][0]["readonly"], false,
            "a plain group leaves its children editable"
        );
    }

    /// A layout wrapper is transparent for naming but is still a field with
    /// its own `admin.readonly`, so it locks the fields it wraps.
    #[test]
    fn a_readonly_layout_wrapper_locks_the_fields_it_wraps() {
        let empty = HashMap::new();

        for field_type in [FieldType::Row, FieldType::Collapsible] {
            let label = format!("{field_type:?}");
            let wrapper = readonly_container(field_type, true);

            let ctx = build_single_field_context(&wrapper, &empty, &empty, "", false, false, 0)
                .to_value();

            assert_eq!(
                ctx["sub_fields"][0]["readonly"], true,
                "{label}: a read-only wrapper locks the fields it wraps"
            );
        }
    }

    /// The same cascade reaches an array's new-row `<template>`: a row added
    /// in a read-only array would otherwise render editable inputs.
    #[test]
    fn a_readonly_array_cascades_readonly_into_its_row_template() {
        let items = FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![text_field("label")])
            .admin(FieldAdmin::builder().readonly(true).build())
            .build();
        let empty = HashMap::new();

        let ctx =
            build_single_field_context(&items, &empty, &empty, "", false, false, 0).to_value();

        assert_eq!(ctx["readonly"], true);
        assert_eq!(
            ctx["sub_fields"][0]["readonly"], true,
            "a template sub-field of a read-only array renders read-only"
        );
    }

    #[test]
    fn non_localized_group_in_non_default_locale_locks_children() {
        let field = group_field("meta", false, vec![text_field("title")]);
        let values = HashMap::new();
        let errors = HashMap::new();

        let ctx =
            build_single_field_context(&field, &values, &errors, "", true, false, 0).to_value();

        // The group itself should be locale-locked
        assert_eq!(ctx["locale_locked"], true);

        // Children should inherit locale lock (non_default_locale=true, group not localized)
        let sub = &ctx["sub_fields"][0];
        assert_eq!(
            sub["locale_locked"], true,
            "child of non-localized group must be locale_locked in non-default locale"
        );
        assert_eq!(sub["readonly"], true);
    }

    /// A locale lock is not an ancestor read-only lock. A non-localized group
    /// renders read-only in a non-default locale, but a *localized* field
    /// inside it is edited in that locale and must stay editable — only
    /// `admin.readonly` cascades.
    #[test]
    fn a_localized_child_of_a_locale_locked_group_stays_editable() {
        let mut title = text_field("title");
        title.localized = true;
        let field = group_field("meta", false, vec![title]);
        let empty = HashMap::new();

        let ctx = build_single_field_context(&field, &empty, &empty, "", true, false, 0).to_value();

        assert_eq!(ctx["locale_locked"], true, "the group itself is locked");
        assert_eq!(ctx["readonly"], true);
        assert_eq!(
            ctx["sub_fields"][0]["locale_locked"], false,
            "a localized child is edited in this locale"
        );
        assert_eq!(
            ctx["sub_fields"][0]["readonly"], false,
            "the group's locale lock must not cascade as a read-only lock"
        );
    }

    #[test]
    fn localized_group_in_non_default_locale_unlocks_children() {
        let field = group_field("meta", true, vec![text_field("title")]);
        let values = HashMap::new();
        let errors = HashMap::new();

        let ctx =
            build_single_field_context(&field, &values, &errors, "", true, false, 0).to_value();

        // The localized group itself should NOT be locale-locked
        assert_eq!(ctx["locale_locked"], false);

        // Children should be editable (non_default_locale reset to false for localized group)
        let sub = &ctx["sub_fields"][0];
        assert_eq!(
            sub["locale_locked"], false,
            "child of localized group must NOT be locale_locked"
        );
        assert_eq!(sub["readonly"], false);
    }

    /// Regression (form parser round-trip): a group nested in an array/blocks
    /// row must index its children as `<group>[0][field]`. The array new-row
    /// template uses an `items[__INDEX__]` prefix; without the `[0]` the form
    /// parser can't parse the group child and drops a new row's group data.
    #[test]
    fn group_in_array_row_indexes_children_with_zero() {
        let field = group_field("meta", false, vec![text_field("author")]);
        let values = HashMap::new();
        let errors = HashMap::new();

        let ctx = build_single_field_context(
            &field,
            &values,
            &errors,
            "items[__INDEX__]",
            false,
            false,
            1,
        )
        .to_value();

        assert_eq!(
            ctx["sub_fields"][0]["name"], "items[__INDEX__][meta][0][author]",
            "group children in an array row need the [0] index the parser requires",
        );
    }

    /// Regression: `admin.hidden` was honored only for top-level fields, so a
    /// hidden checkbox inside a group still got an input. The editor could
    /// uncheck it, but the submit-side normalizer reads a key the form "never
    /// rendered" as "not an edit" — the stored `true` survived the uncheck. The
    /// builder now applies the same answer at every depth.
    #[test]
    fn a_hidden_field_inside_a_group_gets_no_input() {
        let hidden = FieldDefinition::builder("internal", FieldType::Checkbox)
            .admin(FieldAdmin::builder().hidden(true).build())
            .build();
        let field = group_field(
            "meta",
            false,
            vec![text_field("author"), hidden, text_field("note")],
        );
        let empty = HashMap::new();

        let ctx =
            build_single_field_context(&field, &empty, &empty, "", false, false, 0).to_value();

        let names: Vec<&str> = ctx["sub_fields"]
            .as_array()
            .expect("a group renders sub-fields")
            .iter()
            .map(|sf| sf["name"].as_str().expect("each sub-field is named"))
            .collect();

        assert_eq!(
            names,
            vec!["meta__author", "meta__note"],
            "a hidden sub-field must not be rendered, and the surviving ones keep their order"
        );
    }

    /// The same rule one level further in: a hidden field inside a layout
    /// wrapper (transparent, so its children are top-level columns) is not
    /// rendered either.
    #[test]
    fn a_hidden_field_inside_a_layout_wrapper_gets_no_input() {
        let hidden = FieldDefinition::builder("internal", FieldType::Checkbox)
            .admin(FieldAdmin::builder().hidden(true).build())
            .build();
        let field = FieldDefinition::builder("row", FieldType::Row)
            .fields(vec![text_field("title"), hidden])
            .build();
        let empty = HashMap::new();

        let ctx =
            build_single_field_context(&field, &empty, &empty, "", false, false, 0).to_value();

        let subs = ctx["sub_fields"]
            .as_array()
            .expect("a row renders children");
        assert_eq!(subs.len(), 1, "the hidden child is not rendered");
        assert_eq!(subs[0]["name"], "title");
    }

    /// And inside an array's new-row `<template>`: a hidden sub-field would
    /// otherwise reach every JS-added row.
    #[test]
    fn a_hidden_sub_field_is_absent_from_the_array_row_template() {
        let hidden = FieldDefinition::builder("internal", FieldType::Checkbox)
            .admin(FieldAdmin::builder().hidden(true).build())
            .build();
        let field = FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![text_field("label"), hidden])
            .build();
        let empty = HashMap::new();

        let ctx =
            build_single_field_context(&field, &empty, &empty, "", false, false, 0).to_value();

        let subs = ctx["sub_fields"]
            .as_array()
            .expect("an array renders template sub-fields");
        assert_eq!(subs.len(), 1, "the hidden sub-field is not templated");
        assert_eq!(subs[0]["name"], "items[__INDEX__][label]");
    }

    /// A top-level group keeps flat `group__sub` column naming (no `[0]`).
    #[test]
    fn top_level_group_children_use_double_underscore() {
        let field = group_field("meta", false, vec![text_field("author")]);
        let values = HashMap::new();
        let errors = HashMap::new();

        let ctx =
            build_single_field_context(&field, &values, &errors, "", false, false, 0).to_value();

        assert_eq!(ctx["sub_fields"][0]["name"], "meta__author");
    }
}
