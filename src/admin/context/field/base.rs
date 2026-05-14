//! Shared base data flattened into every [`FieldContext`](super::FieldContext)
//! variant. Carries the keys templates expect on every field, regardless of
//! type: `name`, `field_type`, `label`, `required`, `value`, etc.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::core::ConditionExpr;

/// Common keys present on every field context. Variants flatten this into
/// themselves via `#[serde(flatten)]` so the rendered JSON has no nesting.
///
/// `placeholder` and `description` are NOT skipped when None — the existing
/// builder always emits them as `null` so templates that distinguish
/// `null` from `undefined` keep working. (Most templates branch with
/// `{{#if placeholder}}` which treats both identically; the explicit-null
/// form is preserved for parity.)
///
/// **No `field_type` field.** The discriminator is provided by the
/// internally-tagged [`FieldContext`](super::FieldContext) enum.
///
/// `Default` + `#[serde(default)]` are derived so the existing Value-based
/// enrichment code (which constructs ad-hoc sub-field contexts without
/// every base field) can roundtrip through `Deserialize` without panicking
/// on missing keys. The trade-off: typed handlers must explicitly populate
/// fields they care about; missing fields get sensible defaults silently.
#[derive(Serialize, Deserialize, Default, JsonSchema)]
#[serde(default)]
pub struct BaseFieldData {
    /// Form-input name attribute / qualified data-key — the prefixed
    /// path version (e.g. `"seo__rating"` for a rating inside a group,
    /// `"items[0][rating]"` inside an array row). What the browser
    /// submits and what server-side validation keys off.
    pub name: String,

    /// Bare field name as declared on the [`FieldDefinition`], without
    /// any group/array prefix. A field declared as `name = "rating"`
    /// always has `field_name == "rating"` regardless of nesting depth.
    /// Templates use this when they want to match on the
    /// "kind of field" rather than its position in the form (e.g. an
    /// overlay rendering a stars widget for any field literally named
    /// `rating`, whether it lives at the top level or inside a group).
    pub field_name: String,

    pub label: String,
    pub required: bool,
    pub value: Value,
    pub placeholder: Option<String>,
    pub description: Option<String>,
    pub readonly: bool,
    pub localized: bool,
    pub locale_locked: bool,

    /// Where to render this field — `None` for main, `Some("sidebar")` for
    /// the right-hand sidebar.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<String>,

    /// Per-field render template override — set when the field's
    /// `admin.template` is configured. Read by [`RenderFieldHelper`]
    /// to route to a custom template instead of the default
    /// `fields/<field_type>` lookup. Top-level (matching the flatten
    /// convention used by `label`, `placeholder`, etc.) so templates
    /// reference it as `{{template}}` rather than `{{admin.template}}`.
    ///
    /// [`RenderFieldHelper`]: crate::admin::templates::helpers
    #[serde(skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,

    /// Freeform per-field config map — set from the field's
    /// `admin.extra`. Available to the field's render template as
    /// `{{extra.<key>}}` so a custom template can read its config
    /// without forking per field instance. Empty by default.
    #[serde(skip_serializing_if = "Map::is_empty")]
    pub extra: Map<String, Value>,

    /// Validation error message for this field, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,

    /// Validation attribute group, flattened so `min_length`, `max_length`,
    /// `min`, `max`, `has_min`, `has_max` appear at the field-context root
    /// (not nested under `validation`).
    #[serde(flatten)]
    pub validation: ValidationAttrs,

    /// Display-condition data, flattened so `condition_visible`,
    /// `condition_ref`, and `condition_json` appear at the field-context
    /// root.
    #[serde(flatten)]
    pub condition: ConditionData,
}

/// Validation attributes shared by all field types — present only when the
/// field definition declares them.
#[derive(Serialize, Deserialize, Default, JsonSchema)]
pub struct ValidationAttrs {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_length: Option<usize>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_length: Option<usize>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,

    /// Companion flag for `min` — emitted alongside the bound for templates
    /// that branch on presence. Set to `Some(true)` exactly when `min` is
    /// `Some`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_min: Option<bool>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_max: Option<bool>,
}

/// Display-condition state injected by
/// [`apply_display_conditions`](crate::admin::handlers::field_context::apply_display_conditions).
///
/// JSON keys keep the `condition_*` prefix for the JS evaluator at
/// `static/components/conditions.js`; Rust field names drop it because the
/// struct itself is named `ConditionData`.
#[derive(Serialize, Deserialize, Default, JsonSchema)]
pub struct ConditionData {
    /// Initial visibility resolved by the Lua condition function.
    #[serde(rename = "condition_visible", skip_serializing_if = "Option::is_none")]
    pub visible: Option<bool>,

    /// Server-side function reference (set when the condition function
    /// returns a bool). The client re-asks the server when the form changes.
    #[serde(rename = "condition_ref", skip_serializing_if = "Option::is_none")]
    pub func_ref: Option<String>,

    /// Client-evaluable condition expression (set when the condition function
    /// returns a Lua table). The client evaluates this directly without a
    /// round-trip. Serializes to the same JSON shape the JS evaluator at
    /// `static/components/conditions.js` expects.
    #[serde(rename = "condition_json", skip_serializing_if = "Option::is_none")]
    pub expr: Option<ConditionExpr>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admin::context::field::{FieldContext, TextField, test_helpers::make_base};

    #[test]
    fn base_serializes_all_required_keys() {
        let f = TextField {
            base: make_base("title"),
            has_many: None,
            tags: None,
        };
        let v = serde_json::to_value(FieldContext::Text(f)).unwrap();
        // All base keys present (placeholder/description as null per parity).
        assert_eq!(v["name"], "title");
        assert_eq!(v["field_type"], "text");
        assert_eq!(v["label"], "title");
        assert_eq!(v["required"], false);
        assert_eq!(v["value"], "");
        assert!(v["placeholder"].is_null());
        assert!(v["description"].is_null());
        assert_eq!(v["readonly"], false);
        assert_eq!(v["localized"], false);
        assert_eq!(v["locale_locked"], false);
        // Optional keys absent when None.
        assert!(v.get("position").is_none());
        assert!(v.get("error").is_none());
        assert!(v.get("min_length").is_none());
        assert!(v.get("min").is_none());
        assert!(v.get("has_min").is_none());
        assert!(v.get("condition_visible").is_none());
        // Per-field template binding: absent when the field config didn't
        // set `admin.template` / `admin.extra`.
        assert!(v.get("template").is_none(), "template absent by default");
        assert!(v.get("extra").is_none(), "extra absent when empty");
    }

    /// `admin.template` / `admin.extra` declared on the field config flatten
    /// to top-level `template` / `extra` keys in the render context, where
    /// `RenderFieldHelper` reads `template` to override the default
    /// `fields/<field_type>` lookup and field templates read `extra.<key>`
    /// for per-field config.
    #[test]
    fn base_template_and_extra_flatten_to_top_level() {
        let mut b = make_base("rating");
        b.template = Some("fields/rating".to_string());
        b.extra
            .insert("color".to_string(), Value::String("amber".to_string()));
        b.extra.insert("max_stars".to_string(), Value::from(5_i64));

        let f = TextField {
            base: b,
            has_many: None,
            tags: None,
        };
        let v = serde_json::to_value(FieldContext::Text(f)).unwrap();
        assert_eq!(v["template"], "fields/rating");
        assert_eq!(v["extra"]["color"], "amber");
        assert_eq!(v["extra"]["max_stars"], 5);
    }

    #[test]
    fn base_optional_keys_emit_when_set() {
        let mut b = make_base("title");
        b.position = Some("sidebar".to_string());
        b.error = Some("required".to_string());
        b.validation.min_length = Some(3);
        b.validation.max_length = Some(120);
        b.validation.min = Some(1.0);
        b.validation.has_min = Some(true);
        b.condition.visible = Some(true);
        b.condition.func_ref = Some("conditions.is_admin".to_string());

        let f = TextField {
            base: b,
            has_many: None,
            tags: None,
        };
        let v = serde_json::to_value(FieldContext::Text(f)).unwrap();
        assert_eq!(v["position"], "sidebar");
        assert_eq!(v["error"], "required");
        assert_eq!(v["min_length"], 3);
        assert_eq!(v["max_length"], 120);
        assert_eq!(v["min"], 1.0);
        assert_eq!(v["has_min"], true);
        assert_eq!(v["condition_visible"], true);
        assert_eq!(v["condition_ref"], "conditions.is_admin");
    }
}
