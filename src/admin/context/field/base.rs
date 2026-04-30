//! Shared base data flattened into every [`FieldContext`](super::FieldContext)
//! variant. Carries the keys templates expect on every field, regardless of
//! type: `name`, `field_type`, `label`, `required`, `value`, etc.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

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
#[derive(Serialize, Deserialize, Default, JsonSchema)]
pub struct ConditionData {
    /// Initial visibility resolved by the Lua condition function.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub condition_visible: Option<bool>,

    /// Server-side function reference (set when the condition function
    /// returns a bool). The client re-asks the server when the form changes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub condition_ref: Option<String>,

    /// Client-evaluable condition table (set when the condition function
    /// returns a Lua table). The client evaluates this directly without a
    /// round-trip.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub condition_json: Option<Value>,
}
