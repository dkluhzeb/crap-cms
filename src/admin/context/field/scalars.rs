//! Scalar (non-composite, non-reference) field variants:
//! Text, Email, Password, Json, Textarea, Number, Code, Richtext, Date,
//! Checkbox, Select/Radio (Choice).

use serde::Serialize;

use super::BaseFieldData;

// ── Text & friends ────────────────────────────────────────────────

/// Text-like field. Variants: `Text`, `Email`, `Password`, `Json`.
///
/// Only `Text` (and `Number`) supports `has_many` — the others always
/// have `has_many: None` and `tags: None`.
#[derive(Serialize)]
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
#[derive(Serialize)]
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
#[derive(Serialize)]
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
#[derive(Serialize)]
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
#[derive(Serialize)]
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
    #[serde(rename = "_node_names", skip_serializing_if = "Option::is_none")]
    pub node_names: Option<Vec<String>>,
}

// ── Date ──────────────────────────────────────────────────────────

/// Date / datetime picker field.
///
/// Either `date_only_value` (when `picker_appearance == "dayOnly"`) or
/// `datetime_local_value` (when `picker_appearance == "dayAndTime"`) is set
/// — never both. Other appearances emit neither.
#[derive(Serialize)]
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
#[derive(Serialize)]
pub struct TimezoneOption {
    pub value: String,
    pub label: String,
}

// ── Checkbox ──────────────────────────────────────────────────────

/// Boolean checkbox. `checked` is always present.
#[derive(Serialize)]
pub struct CheckboxField {
    #[serde(flatten)]
    pub base: BaseFieldData,

    pub checked: bool,
}

// ── Choice (Select / Radio) ───────────────────────────────────────

/// Select dropdown or radio button group. The `field_type` discriminator
/// on `base` distinguishes the two; the data shape is identical.
#[derive(Serialize)]
pub struct ChoiceField {
    #[serde(flatten)]
    pub base: BaseFieldData,

    pub options: Vec<SelectOption>,

    /// Set to `Some(true)` for multi-select; absent otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_many: Option<bool>,
}

/// One row in a Select/Radio's `options` array.
#[derive(Serialize)]
pub struct SelectOption {
    pub label: String,
    pub value: String,
    pub selected: bool,
}
