//! Composite field variants — fields that contain sub-fields:
//! Group, Row, Collapsible, Tabs, Array, Blocks.
//!
//! All of these hold `Vec<FieldContext>` for their children, making the
//! containing enum recursive. The `Vec` heap indirection keeps the enum
//! sized without `Box`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{BaseFieldData, FieldContext};

// ── Group / Collapsible ───────────────────────────────────────────

/// Inline group of sub-fields with `__`-prefixed column names. Also used
/// for the `Collapsible` variant — they share the exact JSON shape.
#[derive(Serialize, Deserialize, Default, JsonSchema)]
#[serde(default)]
pub struct GroupField {
    #[serde(flatten)]
    pub base: BaseFieldData,

    pub sub_fields: Vec<FieldContext>,

    pub collapsed: bool,
}

impl GroupField {
    /// Construct an uninitialized variant carrying only `base`. Enrichment
    /// populates `sub_fields` (recursive build) + `collapsed` from
    /// `field.admin.collapsed`.
    #[must_use]
    pub fn empty(base: BaseFieldData) -> Self {
        Self {
            base,
            sub_fields: Vec::new(),
            collapsed: false,
        }
    }
}

// ── Row ───────────────────────────────────────────────────────────

/// Layout row wrapper — transparent (no name added to children, no
/// `collapsed` toggle). Distinct from [`GroupField`] only by the absence
/// of `collapsed`.
#[derive(Serialize, Deserialize, Default, JsonSchema)]
#[serde(default)]
pub struct RowField {
    #[serde(flatten)]
    pub base: BaseFieldData,

    pub sub_fields: Vec<FieldContext>,
}

impl RowField {
    /// Construct an uninitialized variant carrying only `base`. Enrichment
    /// recursively builds `sub_fields`.
    #[must_use]
    pub fn empty(base: BaseFieldData) -> Self {
        Self {
            base,
            sub_fields: Vec::new(),
        }
    }
}

// ── Tabs ──────────────────────────────────────────────────────────

/// Tabbed layout wrapper — each tab carries its own sub-fields.
#[derive(Serialize, Deserialize, Default, JsonSchema)]
#[serde(default)]
pub struct TabsField {
    #[serde(flatten)]
    pub base: BaseFieldData,

    pub tabs: Vec<TabPanel>,
}

impl TabsField {
    /// Construct an uninitialized variant carrying only `base`. Enrichment
    /// populates `tabs` by recursing into each declared tab's sub-fields.
    #[must_use]
    pub fn empty(base: BaseFieldData) -> Self {
        Self {
            base,
            tabs: Vec::new(),
        }
    }
}

/// One tab panel inside a [`TabsField`].
#[derive(Serialize, Deserialize, Default, JsonSchema)]
#[serde(default)]
pub struct TabPanel {
    pub label: String,

    pub sub_fields: Vec<FieldContext>,

    /// Number of validation errors inside this tab — emitted only when
    /// non-zero so templates can branch on presence with `{{#if error_count}}`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_count: Option<usize>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

// ── Array ─────────────────────────────────────────────────────────

/// Repeating array of homogeneous rows.
///
/// At builder time, `sub_fields` carries the *template* sub-fields used to
/// render new rows, `rows` is `None`, and `row_count` is `0`. Enrichment
/// fills `rows` from the document data and updates `row_count`.
#[derive(Serialize, Deserialize, Default, JsonSchema)]
#[serde(default)]
pub struct ArrayField {
    #[serde(flatten)]
    pub base: BaseFieldData,

    /// Template sub-fields (used to render new-row UI).
    pub sub_fields: Vec<FieldContext>,

    /// Concrete rows from the document (None pre-enrichment, Some post).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rows: Option<Vec<ArrayRow>>,

    pub row_count: usize,

    /// Sanitised id for use in template `id="…"` attributes.
    pub template_id: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_rows: Option<usize>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_rows: Option<usize>,

    pub init_collapsed: bool,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub add_label: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub label_field: Option<String>,
}

impl ArrayField {
    /// Construct an uninitialized variant carrying only `base` and the
    /// pre-sanitized `template_id` (used by the admin UI for the new-row
    /// template element). Enrichment fills `sub_fields`/`rows`/`row_count`
    /// and the admin-config knobs.
    #[must_use]
    pub fn empty(base: BaseFieldData, template_id: String) -> Self {
        Self {
            base,
            sub_fields: Vec::new(),
            rows: None,
            row_count: 0,
            template_id,
            min_rows: None,
            max_rows: None,
            init_collapsed: false,
            add_label: None,
            label_field: None,
        }
    }
}

/// One concrete row in an [`ArrayField::rows`] list.
#[derive(Serialize, Deserialize, Default, JsonSchema)]
#[serde(default)]
pub struct ArrayRow {
    pub index: usize,
    pub sub_fields: Vec<FieldContext>,

    /// `Some(true)` when at least one sub-field has a validation error;
    /// absent otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_errors: Option<bool>,

    /// Pre-computed row label (from the configured `label_field` or the
    /// `row_label` Lua hook).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_label: Option<String>,
}

// ── Blocks ────────────────────────────────────────────────────────

/// Repeating array of heterogeneous block-typed rows.
///
/// `block_definitions` carries the available block types and their template
/// sub-fields. Enrichment fills `rows` with the concrete block rows from
/// the document.
#[derive(Serialize, Deserialize, Default, JsonSchema)]
#[serde(default)]
pub struct BlocksField {
    #[serde(flatten)]
    pub base: BaseFieldData,

    pub block_definitions: Vec<BlockDefinition>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub rows: Option<Vec<BlockRow>>,

    pub row_count: usize,
    pub template_id: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_rows: Option<usize>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_rows: Option<usize>,

    pub init_collapsed: bool,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub add_label: Option<String>,

    /// Block picker UI style.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub picker: Option<String>,

    /// Optional sub-field name used as the row label in the admin UI.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label_field: Option<String>,
}

impl BlocksField {
    /// Construct an uninitialized variant carrying only `base` and the
    /// pre-sanitized `template_id`. Enrichment fills `block_definitions`
    /// (one per allowed block type), `rows`/`row_count` from the document
    /// data, and the admin-config knobs.
    #[must_use]
    pub fn empty(base: BaseFieldData, template_id: String) -> Self {
        Self {
            base,
            block_definitions: Vec::new(),
            rows: None,
            row_count: 0,
            template_id,
            min_rows: None,
            max_rows: None,
            init_collapsed: false,
            add_label: None,
            picker: None,
            label_field: None,
        }
    }
}

/// One block-type definition inside a [`BlocksField::block_definitions`]
/// array. Carries the template sub-fields used to render a new block of
/// this type.
#[derive(Serialize, Deserialize, Default, JsonSchema)]
#[serde(default)]
pub struct BlockDefinition {
    pub block_type: String,
    pub label: String,

    /// Template sub-fields for this block type.
    pub fields: Vec<FieldContext>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub label_field: Option<String>,

    /// Optional grouping for the block picker UI.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_url: Option<String>,
}

/// One concrete row in a [`BlocksField::rows`] list. Mirrors [`ArrayRow`]
/// but also carries the block discriminator (the `_block_type` JSON key,
/// underscore-prefixed for legacy on-the-wire compatibility).
#[derive(Serialize, Deserialize, Default, JsonSchema)]
#[serde(default)]
pub struct BlockRow {
    pub index: usize,

    /// JSON key is `_block_type` to match the existing template contract.
    #[serde(rename = "_block_type")]
    pub block_type: String,

    /// Display label for the block — defaults to the `block_type` when not
    /// configured. Populated by enrichment.
    pub block_label: String,

    pub sub_fields: Vec<FieldContext>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_errors: Option<bool>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_label: Option<String>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::admin::context::field::{TextField, test_helpers::make_base};

    // ── Group / Collapsible ────────────────────────────────────────────

    #[test]
    fn group_serializes_sub_fields_recursively() {
        let inner = TextField {
            base: make_base("title"),
            has_many: None,
            tags: None,
        };
        let g = GroupField {
            base: make_base("meta"),
            sub_fields: vec![FieldContext::Text(inner)],
            collapsed: false,
        };
        let v = serde_json::to_value(FieldContext::Group(g)).unwrap();
        let subs = v["sub_fields"].as_array().unwrap();
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0]["name"], "title");
        assert_eq!(subs[0]["field_type"], "text");
        assert_eq!(v["collapsed"], false);
    }

    #[test]
    fn collapsible_uses_same_shape_as_group() {
        let g = GroupField {
            base: make_base("meta"),
            sub_fields: vec![],
            collapsed: true,
        };
        let v = serde_json::to_value(FieldContext::Collapsible(g)).unwrap();
        assert_eq!(v["field_type"], "collapsible");
        assert_eq!(v["collapsed"], true);
        assert_eq!(v["sub_fields"], json!([]));
    }

    // ── Row ────────────────────────────────────────────────────────────

    #[test]
    fn row_omits_collapsed_key() {
        let r = RowField {
            base: make_base("layout"),
            sub_fields: vec![],
        };
        let v = serde_json::to_value(FieldContext::Row(r)).unwrap();
        assert!(v.get("collapsed").is_none());
        assert_eq!(v["sub_fields"], json!([]));
    }

    // ── Tabs ───────────────────────────────────────────────────────────

    #[test]
    fn tabs_include_panels_with_optional_error_count() {
        let t = TabsField {
            base: make_base("settings"),
            tabs: vec![
                TabPanel {
                    label: "General".to_string(),
                    sub_fields: vec![],
                    error_count: None,
                    description: None,
                },
                TabPanel {
                    label: "Advanced".to_string(),
                    sub_fields: vec![],
                    error_count: Some(2usize),
                    description: Some("danger zone".to_string()),
                },
            ],
        };
        let v = serde_json::to_value(FieldContext::Tabs(t)).unwrap();
        let tabs = v["tabs"].as_array().unwrap();
        assert_eq!(tabs.len(), 2);
        assert_eq!(tabs[0]["label"], "General");
        assert!(tabs[0].get("error_count").is_none());
        assert_eq!(tabs[1]["error_count"], 2);
        assert_eq!(tabs[1]["description"], "danger zone");
    }

    // ── Array ──────────────────────────────────────────────────────────

    #[test]
    fn array_at_build_time_has_no_rows_and_zero_count() {
        let a = ArrayField {
            base: make_base("items"),
            sub_fields: vec![],
            rows: None,
            row_count: 0,
            template_id: "items".to_string(),
            min_rows: Some(1),
            max_rows: Some(5),
            init_collapsed: false,
            add_label: Some("Item".to_string()),
            label_field: Some("name".to_string()),
        };
        let v = serde_json::to_value(FieldContext::Array(a)).unwrap();
        assert_eq!(v["row_count"], 0);
        assert!(v.get("rows").is_none());
        assert_eq!(v["template_id"], "items");
        assert_eq!(v["min_rows"], 1);
        assert_eq!(v["max_rows"], 5);
        assert_eq!(v["add_label"], "Item");
        assert_eq!(v["label_field"], "name");
    }

    #[test]
    fn array_after_enrichment_has_rows() {
        let row = ArrayRow {
            index: 0,
            sub_fields: vec![],
            has_errors: None,
            custom_label: Some("First row".to_string()),
        };
        let a = ArrayField {
            base: make_base("items"),
            sub_fields: vec![],
            rows: Some(vec![row]),
            row_count: 1,
            template_id: "items".to_string(),
            min_rows: None,
            max_rows: None,
            init_collapsed: false,
            add_label: None,
            label_field: None,
        };
        let v = serde_json::to_value(FieldContext::Array(a)).unwrap();
        assert_eq!(v["row_count"], 1);
        let rows = v["rows"].as_array().unwrap();
        assert_eq!(rows[0]["index"], 0);
        assert_eq!(rows[0]["custom_label"], "First row");
        assert!(rows[0].get("has_errors").is_none());
    }

    // ── Blocks ─────────────────────────────────────────────────────────

    #[test]
    fn blocks_emit_block_definitions() {
        let bd = BlockDefinition {
            block_type: "text".to_string(),
            label: "Text".to_string(),
            fields: vec![],
            label_field: Some("body".to_string()),
            group: None,
            image_url: None,
        };
        let b = BlocksField {
            base: make_base("content"),
            block_definitions: vec![bd],
            rows: None,
            row_count: 0,
            template_id: "content".to_string(),
            min_rows: None,
            max_rows: None,
            init_collapsed: false,
            add_label: None,
            picker: Some("inline".to_string()),
            label_field: None,
        };
        let v = serde_json::to_value(FieldContext::Blocks(b)).unwrap();
        let defs = v["block_definitions"].as_array().unwrap();
        assert_eq!(defs[0]["block_type"], "text");
        assert_eq!(defs[0]["label"], "Text");
        assert_eq!(defs[0]["label_field"], "body");
        assert!(defs[0].get("group").is_none());
        assert_eq!(v["picker"], "inline");
    }

    #[test]
    fn block_row_carries_block_type() {
        let row = BlockRow {
            index: 0,
            block_type: "hero".to_string(),
            block_label: "Hero".to_string(),
            sub_fields: vec![],
            has_errors: Some(true),
            custom_label: None,
        };
        let v = serde_json::to_value(&row).unwrap();
        // JSON key is `_block_type` (underscore-prefixed) per the legacy
        // on-the-wire contract; the Rust field is `block_type`.
        assert_eq!(v["_block_type"], "hero");
        assert_eq!(v["block_label"], "Hero");
        assert_eq!(v["has_errors"], true);
    }
}
