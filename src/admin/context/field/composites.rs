//! Composite field variants — fields that contain sub-fields:
//! Group, Row, Collapsible, Tabs, Array, Blocks.
//!
//! All of these hold `Vec<FieldContext>` for their children, making the
//! containing enum recursive. The `Vec` heap indirection keeps the enum
//! sized without `Box`.

use serde::Serialize;

use super::{BaseFieldData, FieldContext};

// ── Group / Collapsible ───────────────────────────────────────────

/// Inline group of sub-fields with `__`-prefixed column names. Also used
/// for the `Collapsible` variant — they share the exact JSON shape.
#[derive(Serialize)]
pub struct GroupField {
    #[serde(flatten)]
    pub base: BaseFieldData,

    pub sub_fields: Vec<FieldContext>,

    pub collapsed: bool,
}

// ── Row ───────────────────────────────────────────────────────────

/// Layout row wrapper — transparent (no name added to children, no
/// `collapsed` toggle). Distinct from [`GroupField`] only by the absence
/// of `collapsed`.
#[derive(Serialize)]
pub struct RowField {
    #[serde(flatten)]
    pub base: BaseFieldData,

    pub sub_fields: Vec<FieldContext>,
}

// ── Tabs ──────────────────────────────────────────────────────────

/// Tabbed layout wrapper — each tab carries its own sub-fields.
#[derive(Serialize)]
pub struct TabsField {
    #[serde(flatten)]
    pub base: BaseFieldData,

    pub tabs: Vec<TabPanel>,
}

/// One tab panel inside a [`TabsField`].
#[derive(Serialize)]
pub struct TabPanel {
    pub label: String,

    pub sub_fields: Vec<FieldContext>,

    /// Number of validation errors inside this tab — emitted only when
    /// non-zero so templates can branch on presence with `{{#if error_count}}`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_count: Option<u32>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

// ── Array ─────────────────────────────────────────────────────────

/// Repeating array of homogeneous rows.
///
/// At builder time, `sub_fields` carries the *template* sub-fields used to
/// render new rows, `rows` is `None`, and `row_count` is `0`. Enrichment
/// fills `rows` from the document data and updates `row_count`.
#[derive(Serialize)]
pub struct ArrayField {
    #[serde(flatten)]
    pub base: BaseFieldData,

    /// Template sub-fields (used to render new-row UI).
    pub sub_fields: Vec<FieldContext>,

    /// Concrete rows from the document (None pre-enrichment, Some post).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rows: Option<Vec<ArrayRow>>,

    pub row_count: u32,

    /// Sanitised id for use in template `id="…"` attributes.
    pub template_id: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_rows: Option<u32>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_rows: Option<u32>,

    pub init_collapsed: bool,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub add_label: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub label_field: Option<String>,
}

/// One concrete row in an [`ArrayField::rows`] list.
#[derive(Serialize)]
pub struct ArrayRow {
    pub index: u32,
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
#[derive(Serialize)]
pub struct BlocksField {
    #[serde(flatten)]
    pub base: BaseFieldData,

    pub block_definitions: Vec<BlockDefinition>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub rows: Option<Vec<BlockRow>>,

    pub row_count: u32,
    pub template_id: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_rows: Option<u32>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_rows: Option<u32>,

    pub init_collapsed: bool,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub add_label: Option<String>,

    /// Block picker UI style.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub picker: Option<String>,
}

/// One block-type definition inside a [`BlocksField::block_definitions`]
/// array. Carries the template sub-fields used to render a new block of
/// this type.
#[derive(Serialize)]
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
/// but also carries the block discriminator.
#[derive(Serialize)]
pub struct BlockRow {
    pub index: u32,
    pub block_type: String,
    pub sub_fields: Vec<FieldContext>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_errors: Option<bool>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_label: Option<String>,
}
