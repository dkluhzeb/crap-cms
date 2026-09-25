//! The entry point of the single-field build: the field's full form name,
//! the base data every variant shares, and the dispatch to the per-variant
//! constructor.

use std::collections::HashMap;

use serde_json::Value;

use crate::{
    admin::{
        context::field::{
            BaseFieldData, ConditionData, FieldContext, TextField, ValidationAttrs, WidthAttrs,
        },
        handlers::field_context::{
            builder::single::{
                composites::{
                    construct_array, construct_blocks, construct_collapsible, construct_group,
                    construct_row, construct_tabs,
                },
                references::{construct_join, construct_relationship, construct_upload},
                scalars::{
                    construct_checkbox, construct_choice, construct_code, construct_date,
                    construct_json, construct_number, construct_number_tags, construct_richtext,
                    construct_text_tags, construct_textarea,
                },
            },
            cascaded_readonly, locale_locked_display, readonly_display,
        },
    },
    core::{FieldDefinition, FieldType, prefixed_name},
};

/// Resolve the full form name for a field, accounting for layout transparency.
fn resolve_full_name(field: &FieldDefinition, name_prefix: &str) -> String {
    if name_prefix.is_empty() {
        field.name.clone()
    } else if matches!(
        field.field_type,
        FieldType::Tabs | FieldType::Row | FieldType::Collapsible
    ) {
        name_prefix.to_string() // transparent — layout wrappers don't add their name
    } else if !name_prefix.contains('[') {
        // Top-level group chain: continue using __ naming (matches DB columns)
        prefixed_name(name_prefix, &field.name)
    } else {
        format!("{}[{}]", name_prefix, field.name)
    }
}

/// Build the typed common base data shared by every variant. Returns
/// `(base, full_name, value)` so callers can keep using `full_name` and the
/// raw value string for type-specific logic.
fn build_base_field_data(
    field: &FieldDefinition,
    values: &HashMap<String, String>,
    errors: &HashMap<String, String>,
    name_prefix: &str,
    non_default_locale: bool,
    ancestor_readonly: bool,
) -> (BaseFieldData, String, String) {
    let full_name = resolve_full_name(field, name_prefix);
    let value_str = values.get(&full_name).cloned().unwrap_or_default();

    let label = field.resolved_label();

    let locale_locked = locale_locked_display(non_default_locale, field);

    let validation = ValidationAttrs {
        min_length: field.min_length,
        max_length: field.max_length,
        min: field.min,
        max: field.max,
        has_min: field.min.is_some().then_some(true),
        has_max: field.max.is_some().then_some(true),
    };

    let base = BaseFieldData {
        name: full_name.clone(),
        field_name: field.name.clone(),
        label,
        required: field.required,
        value: Value::String(value_str.clone()),
        placeholder: field
            .admin
            .placeholder
            .as_ref()
            .map(|ls| ls.resolve_current().to_string()),
        description: field
            .admin
            .description
            .as_ref()
            .map(|ls| ls.resolve_current().to_string()),
        readonly: readonly_display(field, ancestor_readonly, locale_locked),
        localized: field.localized,
        locale_locked,
        position: field.admin.position.clone(),
        template: field.admin.template.clone(),
        extra: field.admin.extra.clone(),
        error: errors.get(&full_name).cloned(),
        validation,
        layout: WidthAttrs::from_width(field.admin.width.as_ref()),
        condition: ConditionData::default(),
    };

    (base, full_name, value_str)
}

/// Build a typed [`FieldContext`] for a single field definition, recursing
/// into composite sub-fields.
///
/// `name_prefix`: the full form-name prefix for this field (e.g.
/// `"content[0]"` for a field inside a blocks row at index 0). Top-level
/// fields use an empty prefix.
///
/// `ancestor_readonly`: whether a container enclosing this field declares
/// `admin.readonly`. It cascades downward, so a field inside a read-only
/// Group/Array/Blocks/Row/Collapsible/Tabs renders read-only too. Top-level
/// fields pass `false`. A container locked only by the locale does NOT set
/// this — the locale lock is recomputed per field from `non_default_locale`.
///
/// `depth`: current nesting depth (0 = top-level). At
/// [`MAX_FIELD_DEPTH`] the recursion stops and the field is rendered as a
/// minimal text-style fallback (matches the existing behavior of bailing
/// out before type-specific dispatch).
pub fn build_single_field_context(
    field: &FieldDefinition,
    values: &HashMap<String, String>,
    errors: &HashMap<String, String>,
    name_prefix: &str,
    non_default_locale: bool,
    ancestor_readonly: bool,
    depth: usize,
) -> FieldContext {
    let (base, full_name, value_str) = build_base_field_data(
        field,
        values,
        errors,
        name_prefix,
        non_default_locale,
        ancestor_readonly,
    );

    let fc = SingleFieldCtx {
        field,
        value: &value_str,
        values,
        errors,
        name_prefix,
        full_name: &full_name,
        non_default_locale,
        cascade_readonly: cascaded_readonly(field, ancestor_readonly),
        depth,
    };

    construct_field_variant(base, &fc)
}

/// Common params for variant constructors.
pub(super) struct SingleFieldCtx<'a> {
    pub(super) field: &'a FieldDefinition,
    pub(super) value: &'a str,
    pub(super) values: &'a HashMap<String, String>,
    pub(super) errors: &'a HashMap<String, String>,
    pub(super) name_prefix: &'a str,
    pub(super) full_name: &'a str,
    pub(super) non_default_locale: bool,
    /// What every field inside this one inherits: this field's own
    /// `admin.readonly` plus whatever it inherited. Not the rendered
    /// `readonly` — the locale lock is recomputed per field.
    pub(super) cascade_readonly: bool,
    pub(super) depth: usize,
}

/// Dispatch to the appropriate per-variant constructor. Composite variants
/// (Group/Row/Collapsible/Tabs/Array/Blocks) check `fc.depth >=
/// MAX_FIELD_DEPTH` internally and stop recursing rather than building
/// sub-fields. Non-composite variants are unaffected by depth.
fn construct_field_variant(base: BaseFieldData, fc: &SingleFieldCtx) -> FieldContext {
    match &fc.field.field_type {
        FieldType::Text if fc.field.has_many => construct_text_tags(base, fc),
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
        FieldType::Json => construct_json(base, fc),
        FieldType::Textarea => construct_textarea(base, fc),
        FieldType::Number if fc.field.has_many => construct_number_tags(base, fc),
        FieldType::Number => construct_number(base, fc),
        FieldType::Code => construct_code(base, fc),
        FieldType::Richtext => construct_richtext(base, fc),
        FieldType::Date => construct_date(base, fc),
        FieldType::Checkbox => construct_checkbox(base, fc),
        FieldType::Select => construct_choice(base, fc, FieldContext::Select),
        FieldType::Radio => construct_choice(base, fc, FieldContext::Radio),
        FieldType::Relationship => construct_relationship(base, fc),
        FieldType::Upload => construct_upload(base, fc),
        FieldType::Join => construct_join(base, fc),
        FieldType::Group => construct_group(base, fc),
        FieldType::Row => construct_row(base, fc),
        FieldType::Collapsible => construct_collapsible(base, fc),
        FieldType::Tabs => construct_tabs(base, fc),
        FieldType::Array => construct_array(base, fc),
        FieldType::Blocks => construct_blocks(base, fc),
    }
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

    /// `admin.readonly` reaches the context as `readonly` for every field type,
    /// including the two whose templates used to branch on `locale_locked`
    /// alone and so rendered an editable Checkbox / Select. `locale_locked`
    /// stays the narrower flag — a readonly field is not locale-locked.
    #[test]
    fn admin_readonly_reaches_the_context_for_every_field_type() {
        let empty = HashMap::new();

        for field_type in [FieldType::Checkbox, FieldType::Select, FieldType::Text] {
            let label = format!("{field_type:?}");
            let field = FieldDefinition::builder("flag", field_type)
                .admin(FieldAdmin::builder().readonly(true).build())
                .build();

            let fc = build_single_field_context(&field, &empty, &empty, "", false, false, 0);

            assert!(fc.base().readonly, "{label} must render readonly");
            assert!(
                !fc.base().locale_locked,
                "{label}: readonly is not the same as locale-locked"
            );
        }
    }
}
