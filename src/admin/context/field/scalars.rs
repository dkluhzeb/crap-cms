//! Scalar (non-composite, non-reference) field variants:
//! Text, Email, Password, Json, Textarea, Number, Code, Richtext, Date,
//! Checkbox, Select/Radio (Choice).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::BaseFieldData;

// ── Text & friends ────────────────────────────────────────────────

/// Text-like field. Variants: `Text`, `Email`, `Password`, `Json`.
///
/// Only `Text` (and `Number`) supports `has_many` — the others always
/// have `has_many: None` and `tags: None`.
#[derive(Serialize, Deserialize, Default, JsonSchema)]
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

impl TextField {
    /// Construct an uninitialized variant carrying only `base` — enrichment
    /// populates `has_many` / `tags` later.
    #[must_use]
    pub fn empty(base: BaseFieldData) -> Self {
        Self {
            base,
            has_many: None,
            tags: None,
        }
    }
}

// ── Textarea ──────────────────────────────────────────────────────

/// Multi-line textarea. Always emits `rows` and `resizable`.
#[derive(Serialize, Deserialize, Default, JsonSchema)]
#[serde(default)]
pub struct TextareaField {
    #[serde(flatten)]
    pub base: BaseFieldData,

    /// Number of visible text rows.
    pub rows: u32,

    /// Whether the textarea allows user-resizing in the admin UI.
    pub resizable: bool,
}

impl TextareaField {
    /// Construct with `base` and field-level admin defaults (`rows = 8`,
    /// `resizable = false`). Enrichment overwrites these from `field.admin`.
    #[must_use]
    pub fn empty(base: BaseFieldData) -> Self {
        Self {
            base,
            rows: 8,
            resizable: false,
        }
    }
}

// ── Number ────────────────────────────────────────────────────────

/// Numeric input. `step` is always emitted (default `"any"`).
#[derive(Serialize, Deserialize, Default, JsonSchema)]
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

impl NumberField {
    /// Construct an uninitialized variant carrying only `base`. Enrichment
    /// populates `step` and `has_many`/`tags`.
    #[must_use]
    pub fn empty(base: BaseFieldData) -> Self {
        Self {
            base,
            step: String::new(),
            has_many: None,
            tags: None,
        }
    }
}

// ── Code ──────────────────────────────────────────────────────────

/// Source-code editor field (`CodeMirror`). Always emits `language`. Emits
/// `languages` only when the operator configured an allow-list (which makes
/// the editor render an in-form picker).
#[derive(Serialize, Deserialize, Default, JsonSchema)]
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

impl CodeField {
    /// Construct an uninitialized variant carrying only `base`. Enrichment
    /// populates `language` / `languages` from the field's admin config.
    #[must_use]
    pub fn empty(base: BaseFieldData) -> Self {
        Self {
            base,
            language: String::new(),
            languages: None,
        }
    }
}

// ── Richtext ──────────────────────────────────────────────────────

/// Rich-text editor field (`ProseMirror`). The `_node_names` key is prefixed
/// with `_` per the existing on-the-wire shape consumed by the
/// `<crap-richtext>` element.
#[derive(Serialize, Deserialize, Default, JsonSchema)]
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

    /// Optional list of allowed `ProseMirror` node names. Emitted with a
    /// leading underscore per the existing client-side contract.
    /// Removed from the JSON by enrichment (replaced by [`Self::custom_nodes`]).
    #[serde(rename = "_node_names", skip_serializing_if = "Option::is_none")]
    pub node_names: Option<Vec<String>>,

    /// Resolved custom node definitions — populated by enrichment from the
    /// names in [`Self::node_names`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_nodes: Option<Vec<RichtextNodeDefCtx>>,
}

impl RichtextField {
    /// Construct with `base` and the wire-format default
    /// (`richtext_format = "html"`). Enrichment overwrites the format
    /// + populates `features` / `node_names` / `custom_nodes`.
    #[must_use]
    pub fn empty(base: BaseFieldData) -> Self {
        Self {
            base,
            resizable: false,
            richtext_format: "html".to_string(),
            features: None,
            node_names: None,
            custom_nodes: None,
        }
    }
}

/// One custom `ProseMirror` node definition exposed to the richtext editor.
#[derive(Serialize, Deserialize, Default, JsonSchema)]
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
#[derive(Serialize, Deserialize, Default, JsonSchema)]
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
#[derive(Serialize, Deserialize, Default, JsonSchema)]
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
#[derive(Serialize, Deserialize, Default, JsonSchema)]
#[serde(default)]
pub struct DateField {
    #[serde(flatten)]
    pub base: BaseFieldData,

    /// One of `"dayOnly"`, `"dayAndTime"`, `"timeOnly"`, `"monthOnly"`.
    /// Defaults to `"dayOnly"`. (`timeOnly`/`monthOnly` set neither
    /// `date_only_value` nor `datetime_local_value`; the template falls
    /// back to the raw `value`.)
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

impl DateField {
    /// Construct with `base` and the default picker appearance (`"dayOnly"`).
    /// Enrichment overwrites the appearance + populates min/max/timezone keys
    /// from the field's admin config.
    #[must_use]
    pub fn empty(base: BaseFieldData) -> Self {
        Self {
            base,
            picker_appearance: "dayOnly".to_string(),
            date_only_value: None,
            datetime_local_value: None,
            min_date: None,
            max_date: None,
            timezone_enabled: None,
            default_timezone: None,
            timezone_options: None,
            timezone_value: None,
        }
    }
}

/// One row in a Date field's timezone picker.
#[derive(Serialize, Deserialize, Default, JsonSchema)]
#[serde(default)]
pub struct TimezoneOption {
    pub value: String,
    pub label: String,
}

// ── Checkbox ──────────────────────────────────────────────────────

/// Boolean checkbox. `checked` is always present.
#[derive(Serialize, Deserialize, Default, JsonSchema)]
#[serde(default)]
pub struct CheckboxField {
    #[serde(flatten)]
    pub base: BaseFieldData,

    pub checked: bool,
}

impl CheckboxField {
    /// Construct with `base` and `checked = false`. Enrichment populates
    /// `checked` from the field value.
    #[must_use]
    pub fn empty(base: BaseFieldData) -> Self {
        Self {
            base,
            checked: false,
        }
    }
}

// ── Choice (Select / Radio) ───────────────────────────────────────

/// Select dropdown or radio button group. The `field_type` discriminator
/// on `base` distinguishes the two; the data shape is identical.
#[derive(Serialize, Deserialize, Default, JsonSchema)]
#[serde(default)]
pub struct ChoiceField {
    #[serde(flatten)]
    pub base: BaseFieldData,

    pub options: Vec<SelectOption>,

    /// Set to `Some(true)` for multi-select; absent otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_many: Option<bool>,
}

impl ChoiceField {
    /// Construct an uninitialized variant carrying only `base`. Enrichment
    /// populates `options` + `has_many` from the field's admin config.
    #[must_use]
    pub fn empty(base: BaseFieldData) -> Self {
        Self {
            base,
            options: Vec::new(),
            has_many: None,
        }
    }
}

/// One row in a Select/Radio's `options` array.
#[derive(Serialize, Deserialize, Default, JsonSchema)]
#[serde(default)]
pub struct SelectOption {
    pub label: String,
    pub value: String,
    pub selected: bool,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::admin::context::field::{FieldContext, test_helpers::make_base};

    // ── Text-like variants ─────────────────────────────────────────────

    #[test]
    fn text_with_has_many_emits_tags() {
        let f = TextField {
            base: make_base("tags"),
            has_many: Some(true),
            tags: Some(vec!["rust".to_string(), "cms".to_string()]),
        };
        let v = serde_json::to_value(FieldContext::Text(f)).unwrap();
        assert_eq!(v["has_many"], true);
        assert_eq!(v["tags"], json!(["rust", "cms"]));
    }

    #[test]
    fn text_without_has_many_omits_keys() {
        let f = TextField {
            base: make_base("title"),
            has_many: None,
            tags: None,
        };
        let v = serde_json::to_value(FieldContext::Text(f)).unwrap();
        assert!(v.get("has_many").is_none());
        assert!(v.get("tags").is_none());
    }

    #[test]
    fn email_variant_uses_email_field_type() {
        let f = TextField {
            base: make_base("contact"),
            has_many: None,
            tags: None,
        };
        let v = serde_json::to_value(FieldContext::Email(f)).unwrap();
        assert_eq!(v["field_type"], "email");
    }

    // ── Textarea ───────────────────────────────────────────────────────

    #[test]
    fn textarea_always_emits_rows_and_resizable() {
        let f = TextareaField {
            base: make_base("body"),
            rows: 8,
            resizable: true,
        };
        let v = serde_json::to_value(FieldContext::Textarea(f)).unwrap();
        assert_eq!(v["rows"], 8);
        assert_eq!(v["resizable"], true);
    }

    // ── Number ─────────────────────────────────────────────────────────

    #[test]
    fn number_always_emits_step() {
        let f = NumberField {
            base: make_base("count"),
            step: "any".to_string(),
            has_many: None,
            tags: None,
        };
        let v = serde_json::to_value(FieldContext::Number(f)).unwrap();
        assert_eq!(v["step"], "any");
        assert!(v.get("has_many").is_none());
    }

    // ── Code ───────────────────────────────────────────────────────────

    #[test]
    fn code_emits_language_and_optional_languages() {
        let f = CodeField {
            base: make_base("snippet"),
            language: "javascript".to_string(),
            languages: Some(vec!["javascript".to_string(), "python".to_string()]),
        };
        let v = serde_json::to_value(FieldContext::Code(f)).unwrap();
        assert_eq!(v["language"], "javascript");
        assert_eq!(v["languages"], json!(["javascript", "python"]));
    }

    #[test]
    fn code_without_languages_omits_picker_key() {
        let f = CodeField {
            base: make_base("snippet"),
            language: "json".to_string(),
            languages: None,
        };
        let v = serde_json::to_value(FieldContext::Code(f)).unwrap();
        assert_eq!(v["language"], "json");
        assert!(v.get("languages").is_none());
    }

    // ── Richtext ───────────────────────────────────────────────────────

    #[test]
    fn richtext_renames_node_names_with_underscore_prefix() {
        let f = RichtextField {
            base: make_base("body"),
            resizable: false,
            richtext_format: "html".to_string(),
            features: Some(vec!["bold".to_string()]),
            node_names: Some(vec!["paragraph".to_string()]),
            custom_nodes: None,
        };
        let v = serde_json::to_value(FieldContext::Richtext(f)).unwrap();
        // Per the existing on-the-wire shape consumed by <crap-richtext>.
        assert_eq!(v["_node_names"], json!(["paragraph"]));
        assert_eq!(v["features"], json!(["bold"]));
        assert_eq!(v["richtext_format"], "html");
    }

    // ── Date ───────────────────────────────────────────────────────────

    #[test]
    fn date_day_only_sets_date_only_value() {
        let f = DateField {
            base: make_base("published"),
            picker_appearance: "dayOnly".to_string(),
            date_only_value: Some("2026-01-15".to_string()),
            datetime_local_value: None,
            min_date: None,
            max_date: None,
            timezone_enabled: None,
            default_timezone: None,
            timezone_options: None,
            timezone_value: None,
        };
        let v = serde_json::to_value(FieldContext::Date(f)).unwrap();
        assert_eq!(v["picker_appearance"], "dayOnly");
        assert_eq!(v["date_only_value"], "2026-01-15");
        assert!(v.get("datetime_local_value").is_none());
    }

    #[test]
    fn date_with_timezone_emits_picker_keys() {
        let f = DateField {
            base: make_base("published"),
            picker_appearance: "dayAndTime".to_string(),
            date_only_value: None,
            datetime_local_value: Some("2026-01-15T09:30".to_string()),
            min_date: None,
            max_date: None,
            timezone_enabled: Some(true),
            default_timezone: Some("America/New_York".to_string()),
            timezone_options: Some(vec![TimezoneOption {
                value: "UTC".to_string(),
                label: "UTC".to_string(),
            }]),
            timezone_value: Some("Europe/Berlin".to_string()),
        };
        let v = serde_json::to_value(FieldContext::Date(f)).unwrap();
        assert_eq!(v["timezone_enabled"], true);
        assert_eq!(v["default_timezone"], "America/New_York");
        assert_eq!(v["timezone_value"], "Europe/Berlin");
        assert_eq!(v["timezone_options"][0]["value"], "UTC");
    }

    // ── Choice (Select / Radio) ────────────────────────────────────────

    #[test]
    fn select_emits_options_and_optional_has_many() {
        let f = ChoiceField {
            base: make_base("color"),
            options: vec![
                SelectOption {
                    label: "Red".to_string(),
                    value: "red".to_string(),
                    selected: false,
                },
                SelectOption {
                    label: "Green".to_string(),
                    value: "green".to_string(),
                    selected: true,
                },
            ],
            has_many: None,
        };
        let v = serde_json::to_value(FieldContext::Select(f)).unwrap();
        let opts = v["options"].as_array().unwrap();
        assert_eq!(opts.len(), 2);
        assert_eq!(opts[0]["label"], "Red");
        assert_eq!(opts[1]["selected"], true);
        assert!(v.get("has_many").is_none());
    }

    // ── Checkbox ───────────────────────────────────────────────────────

    #[test]
    fn checkbox_emits_checked() {
        let f = CheckboxField {
            base: make_base("active"),
            checked: true,
        };
        let v = serde_json::to_value(FieldContext::Checkbox(f)).unwrap();
        assert_eq!(v["checked"], true);
    }
}
