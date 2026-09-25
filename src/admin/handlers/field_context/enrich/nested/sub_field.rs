//! Builds the enriched context of one sub-field inside an array/blocks row:
//! its indexed form name, its stringified value, its shared base data, and
//! the recursion into its type-specific enrichment.

use serde_json::Value;

use crate::{
    admin::{
        context::field::{BaseFieldData, ConditionData, FieldContext, ValidationAttrs, WidthAttrs},
        handlers::field_context::{
            MAX_FIELD_DEPTH, cascaded_readonly,
            enrich::{
                SubFieldOpts,
                nested::dispatch::{construct_sub_variant, dispatch_sub_field_type},
            },
            json_textarea_value, locale_locked_display, readonly_display,
        },
    },
    core::{FieldDefinition, FieldType},
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
/// Scalar types get their string representation — a JSON field's value
/// pretty-printed for its textarea; composite types return empty string
/// since their structure is handled recursively.
fn stringify_sub_field_value(raw_value: Option<&Value>, sf: &FieldDefinition) -> String {
    raw_value
        .map(|v| match v {
            Value::String(s) if sf.field_type == FieldType::Json => json_textarea_value(s),
            Value::String(s) => s.clone(),
            Value::Null => String::new(),
            other => match sf.field_type {
                FieldType::Array
                | FieldType::Blocks
                | FieldType::Group
                | FieldType::Row
                | FieldType::Collapsible
                | FieldType::Tabs => String::new(),
                FieldType::Json => json_textarea_value(&other.to_string()),
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
    let sf_label = sf.resolved_label();

    // Recompute per-field instead of inheriting from the parent: a localized
    // field inside a non-localized parent must stay editable in non-default
    // locales. Layout wrappers (Row/Tabs/Collapsible) themselves are always
    // non-localized, so they pick up the parent's lock state naturally via
    // this same formula.
    let locale_locked = locale_locked_display(opts.non_default_locale, sf);

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
            .map(|ls| ls.resolve_current().to_string()),
        description: sf
            .admin
            .description
            .as_ref()
            .map(|ls| ls.resolve_current().to_string()),
        readonly: readonly_display(sf, opts.ancestor_readonly, locale_locked),
        localized: sf.localized,
        locale_locked,
        position: sf.admin.position.clone(),
        template: sf.admin.template.clone(),
        extra: sf.admin.extra.clone(),
        error: opts.errors.get(indexed_name).cloned(),
        validation: ValidationAttrs::default(),
        layout: WidthAttrs::from_width(sf.admin.width.as_ref()),
        condition: ConditionData::default(),
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

    // `admin.readonly` cascades: everything nested inside a read-only
    // container renders read-only too. The locale lock is not carried here —
    // it is recomputed per field from `non_default_locale`.
    let inner_opts = SubFieldOpts::builder(opts.errors)
        .locale_locked(opts.locale_locked)
        .non_default_locale(opts.non_default_locale)
        .ancestor_readonly(cascaded_readonly(sf, opts.ancestor_readonly))
        .depth(opts.depth)
        .build();

    let mut fc = construct_sub_variant(sf, base, &indexed_name);

    if opts.depth < MAX_FIELD_DEPTH {
        dispatch_sub_field_type(&mut fc, sf, &val, raw_value, &indexed_name, &inner_opts);
    }

    fc
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;

    use super::*;
    use crate::admin::handlers::field_context::enrich::test_helpers::{
        build_enriched_sub_field_value, make_field,
    };

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

    /// A read-only Array locks every field in every row it holds, and in the
    /// new-row template. `admin.readonly` on a container is not a label on the
    /// container alone — it cascades into what the container contains.
    #[test]
    fn a_readonly_array_cascades_readonly_into_its_rows() {
        let mut items = make_field("items", FieldType::Array);
        items.admin.readonly = true;
        items.fields = vec![make_field("title", FieldType::Text)];

        let rows = json!([{ "title": "Hello" }]);

        let ctx = build_enriched_sub_field_value(
            &items,
            Some(&rows),
            "page",
            0,
            &SubFieldOpts::builder(&HashMap::new()).depth(1).build(),
        );

        assert_eq!(ctx["readonly"], true);

        let row_field = &ctx["rows"][0]["sub_fields"][0];
        assert_eq!(
            row_field["readonly"], true,
            "a row's field inherits the array's read-only state"
        );
        assert_eq!(
            row_field["locale_locked"], false,
            "read-only is not the same as locale-locked"
        );
        assert_eq!(
            ctx["sub_fields"][0]["readonly"], true,
            "the new-row template inherits it too"
        );
    }

    /// The control: a plain Array leaves the fields in its rows editable.
    #[test]
    fn a_plain_array_leaves_its_row_fields_editable() {
        let mut items = make_field("items", FieldType::Array);
        items.fields = vec![make_field("title", FieldType::Text)];

        let rows = json!([{ "title": "Hello" }]);

        let ctx = build_enriched_sub_field_value(
            &items,
            Some(&rows),
            "page",
            0,
            &SubFieldOpts::builder(&HashMap::new()).depth(1).build(),
        );

        assert_eq!(ctx["readonly"], false);
        assert_eq!(ctx["rows"][0]["sub_fields"][0]["readonly"], false);
    }

    /// The same for a Group: a read-only group locks its sub-fields, however
    /// deep the nesting goes.
    #[test]
    fn a_readonly_group_cascades_readonly_to_its_sub_fields() {
        let mut meta = make_field("meta", FieldType::Group);
        meta.admin.readonly = true;
        meta.fields = vec![make_field("author", FieldType::Text)];

        let value = json!({ "author": "Alice" });

        let ctx = build_enriched_sub_field_value(
            &meta,
            Some(&value),
            "items",
            0,
            &SubFieldOpts::builder(&HashMap::new()).depth(1).build(),
        );

        assert_eq!(ctx["readonly"], true);
        assert_eq!(
            ctx["sub_fields"][0]["readonly"], true,
            "a read-only group locks the fields inside it"
        );
        assert_eq!(ctx["sub_fields"][0]["locale_locked"], false);
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
}
