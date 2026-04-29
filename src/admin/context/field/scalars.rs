//! Scalar (non-composite, non-reference) field variants:
//! Text, Email, Password, Json, Textarea, Number, Code, Richtext, Date,
//! Checkbox, Select/Radio (Choice).

use serde::{Deserialize, Serialize};

use super::BaseFieldData;

// ── Text & friends ────────────────────────────────────────────────

/// Text-like field. Variants: `Text`, `Email`, `Password`, `Json`.
///
/// Only `Text` (and `Number`) supports `has_many` — the others always
/// have `has_many: None` and `tags: None`.
#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TextField {
    #[serde(flatten)]
    pub base: BaseFieldData,

    /// Set to `Some(true)` when the field is configured as a tag list.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_many: Option<bool>,

    /// Parsed tag list (when `has_many` is true; absent otherwise).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
}

// ── Textarea ──────────────────────────────────────────────────────

/// Multi-line textarea. Always emits `rows` and `resizable`.
#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TextareaField {
    #[serde(flatten)]
    pub base: BaseFieldData,

    /// Number of visible text rows.
    pub rows: u32,

    /// Whether the textarea allows user-resizing in the admin UI.
    pub resizable: bool,
}

// ── Number ────────────────────────────────────────────────────────

/// Numeric input. `step` is always emitted (default `"any"`).
#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
pub struct NumberField {
    #[serde(flatten)]
    pub base: BaseFieldData,

    /// HTML `step` attribute. `"any"` allows arbitrary precision.
    pub step: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_many: Option<bool>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
}

// ── Code ──────────────────────────────────────────────────────────

/// Source-code editor field (CodeMirror). Always emits `language`. Emits
/// `languages` only when the operator configured an allow-list (which makes
/// the editor render an in-form picker).
#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
pub struct CodeField {
    #[serde(flatten)]
    pub base: BaseFieldData,

    /// Editor language (e.g. `"json"`, `"javascript"`).
    pub language: String,

    /// Optional allow-list — when present, the admin UI renders a language
    /// picker and a hidden `_lang` companion input.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub languages: Option<Vec<String>>,
}

// ── Richtext ──────────────────────────────────────────────────────

/// Rich-text editor field (ProseMirror). The `_node_names` key is prefixed
/// with `_` per the existing on-the-wire shape consumed by the
/// `<crap-richtext>` element.
#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
pub struct RichtextField {
    #[serde(flatten)]
    pub base: BaseFieldData,

    /// Whether the editor is user-resizable.
    pub resizable: bool,

    /// Storage format. Currently `"html"` or `"json"`. Always emitted; the
    /// builder defaults to `"html"`.
    pub richtext_format: String,

    /// Optional list of enabled toolbar features.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub features: Option<Vec<String>>,

    /// Optional list of allowed ProseMirror node names. Emitted with a
    /// leading underscore per the existing client-side contract.
    /// Removed from the JSON by enrichment (replaced by [`Self::custom_nodes`]).
    #[serde(rename = "_node_names", skip_serializing_if = "Option::is_none")]
    pub node_names: Option<Vec<String>>,

    /// Resolved custom node definitions — populated by enrichment from the
    /// names in [`Self::node_names`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_nodes: Option<Vec<RichtextNodeDefCtx>>,
}

/// One custom ProseMirror node definition exposed to the richtext editor.
#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
pub struct RichtextNodeDefCtx {
    pub name: String,
    pub label: String,
    pub inline: bool,
    pub attrs: Vec<RichtextNodeAttrCtx>,
}

/// One attribute on a custom richtext node — describes a form field rendered
/// in the node-edit modal. Many fields are optional and only emitted when
/// configured.
#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
pub struct RichtextNodeAttrCtx {
    pub name: String,
    /// The HTML form-field type discriminator (`text`, `number`, `select`, …).
    /// Renamed because `type` is a Rust keyword.
    #[serde(rename = "type")]
    pub kind: String,
    pub label: String,
    pub required: bool,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<RichtextNodeAttrOption>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub hidden: Option<bool>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub readonly: Option<bool>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub rows: Option<u32>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_length: Option<usize>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_length: Option<usize>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_date: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_date: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub picker_appearance: Option<String>,
}

/// One row in a richtext node attribute's `options` list (Select/Radio attrs).
#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
pub struct RichtextNodeAttrOption {
    pub label: String,
    pub value: String,
}

// ── Date ──────────────────────────────────────────────────────────

/// Date / datetime picker field.
///
/// Either `date_only_value` (when `picker_appearance == "dayOnly"`) or
/// `datetime_local_value` (when `picker_appearance == "dayAndTime"`) is set
/// — never both. Other appearances emit neither.
#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
pub struct DateField {
    #[serde(flatten)]
    pub base: BaseFieldData,

    /// One of `"dayOnly"`, `"dayAndTime"`. Defaults to `"dayOnly"`.
    pub picker_appearance: String,

    /// Set when `picker_appearance == "dayOnly"` — the `YYYY-MM-DD` slice.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date_only_value: Option<String>,

    /// Set when `picker_appearance == "dayAndTime"` — the
    /// `YYYY-MM-DDTHH:MM` slice for the `<input type="datetime-local">`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub datetime_local_value: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_date: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_date: Option<String>,

    // Timezone keys — only emitted when the field has `timezone: true`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timezone_enabled: Option<bool>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_timezone: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub timezone_options: Option<Vec<TimezoneOption>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub timezone_value: Option<String>,
}

/// One row in a Date field's timezone picker.
#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TimezoneOption {
    pub value: String,
    pub label: String,
}

// ── Checkbox ──────────────────────────────────────────────────────

/// Boolean checkbox. `checked` is always present.
#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
pub struct CheckboxField {
    #[serde(flatten)]
    pub base: BaseFieldData,

    pub checked: bool,
}

// ── Choice (Select / Radio) ───────────────────────────────────────

/// Select dropdown or radio button group. The `field_type` discriminator
/// on `base` distinguishes the two; the data shape is identical.
#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ChoiceField {
    #[serde(flatten)]
    pub base: BaseFieldData,

    pub options: Vec<SelectOption>,

    /// Set to `Some(true)` for multi-select; absent otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_many: Option<bool>,
}

/// One row in a Select/Radio's `options` array.
#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SelectOption {
    pub label: String,
    pub value: String,
    pub selected: bool,
}
