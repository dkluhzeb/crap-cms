//! The `FieldContext` enum + `base()` / `base_mut()` accessors.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
#[cfg(test)]
use serde_json::Value;

use super::{
    base::BaseFieldData,
    composites::{ArrayField, BlocksField, GroupField, RowField, TabsField},
    refs::{JoinField, RelationshipField, UploadField},
    scalars::{
        CheckboxField, ChoiceField, CodeField, DateField, NumberField, RichtextField, TextField,
        TextareaField,
    },
};

/// Typed admin form field context — one variant per
/// [`FieldType`](crate::core::FieldType).
///
/// Internally tagged on `field_type` (lowercase variant name) so the
/// serialized JSON has `{"field_type": "text", ...flat fields...}`. This is
/// the single source of truth for the discriminator — [`BaseFieldData`]
/// does NOT carry a `field_type` field.
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(tag = "field_type", rename_all = "lowercase")]
#[schemars(rename_all = "lowercase")]
pub enum FieldContext {
    /// Plain text input (or tag input when `has_many`).
    Text(TextField),
    /// Email address input (validated client-side as `type=email`).
    Email(TextField),
    /// Password input — synthetic, used for auth-collection forms.
    Password(TextField),
    /// Free-form JSON input.
    Json(TextField),
    /// Multi-line textarea.
    Textarea(TextareaField),
    /// Numeric input (or tag input when `has_many`).
    Number(NumberField),
    /// Source-code editor (`CodeMirror`).
    Code(CodeField),
    /// Rich-text editor (`ProseMirror`).
    Richtext(RichtextField),
    /// Date / datetime picker.
    Date(DateField),
    /// Single boolean checkbox.
    Checkbox(CheckboxField),
    /// Select dropdown.
    Select(ChoiceField),
    /// Radio button group.
    Radio(ChoiceField),
    /// Reference to another collection's documents.
    Relationship(RelationshipField),
    /// Upload field (specialised relationship to media collection).
    Upload(UploadField),
    /// Read-only join field (computed inverse relationship).
    Join(JoinField),
    /// Inline group of sub-fields (with `__` column-name prefix).
    Group(GroupField),
    /// Layout-only row wrapper (transparent — no name added).
    Row(RowField),
    /// Layout collapsible wrapper (transparent + `collapsed`).
    Collapsible(GroupField),
    /// Layout tabbed wrapper (each tab has its own sub-fields).
    Tabs(TabsField),
    /// Repeating array of homogeneous rows.
    Array(ArrayField),
    /// Repeating array of heterogeneous block-typed rows.
    Blocks(BlocksField),
}

impl FieldContext {
    /// Convert this field context to its JSON representation. Infallible —
    /// admin context structs serialize cleanly. Test-only: production code
    /// serializes via the typed pipeline, but tests use this for assertions
    /// against the wire JSON shape.
    #[cfg(test)]
    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).expect("FieldContext serialization is infallible")
    }

    /// Borrow the shared base data of this field context, regardless of variant.
    pub fn base(&self) -> &BaseFieldData {
        match self {
            FieldContext::Text(f)
            | FieldContext::Email(f)
            | FieldContext::Password(f)
            | FieldContext::Json(f) => &f.base,
            FieldContext::Textarea(f) => &f.base,
            FieldContext::Number(f) => &f.base,
            FieldContext::Code(f) => &f.base,
            FieldContext::Richtext(f) => &f.base,
            FieldContext::Date(f) => &f.base,
            FieldContext::Checkbox(f) => &f.base,
            FieldContext::Select(f) | FieldContext::Radio(f) => &f.base,
            FieldContext::Relationship(f) => &f.base,
            FieldContext::Upload(f) => &f.base,
            FieldContext::Join(f) => &f.base,
            FieldContext::Group(f) | FieldContext::Collapsible(f) => &f.base,
            FieldContext::Row(f) => &f.base,
            FieldContext::Tabs(f) => &f.base,
            FieldContext::Array(f) => &f.base,
            FieldContext::Blocks(f) => &f.base,
        }
    }

    /// Mutably borrow the shared base data. Used by post-build enrichers
    /// (display conditions, error injection) that need to mutate base
    /// fields without caring about the variant.
    pub fn base_mut(&mut self) -> &mut BaseFieldData {
        match self {
            FieldContext::Text(f)
            | FieldContext::Email(f)
            | FieldContext::Password(f)
            | FieldContext::Json(f) => &mut f.base,
            FieldContext::Textarea(f) => &mut f.base,
            FieldContext::Number(f) => &mut f.base,
            FieldContext::Code(f) => &mut f.base,
            FieldContext::Richtext(f) => &mut f.base,
            FieldContext::Date(f) => &mut f.base,
            FieldContext::Checkbox(f) => &mut f.base,
            FieldContext::Select(f) | FieldContext::Radio(f) => &mut f.base,
            FieldContext::Relationship(f) => &mut f.base,
            FieldContext::Upload(f) => &mut f.base,
            FieldContext::Join(f) => &mut f.base,
            FieldContext::Group(f) | FieldContext::Collapsible(f) => &mut f.base,
            FieldContext::Row(f) => &mut f.base,
            FieldContext::Tabs(f) => &mut f.base,
            FieldContext::Array(f) => &mut f.base,
            FieldContext::Blocks(f) => &mut f.base,
        }
    }

    /// Read-only sub-field slices of this rendered field — the `FieldContext`
    /// analogue of `core::walk::field_children`. ONE exhaustive match, so a
    /// new composite variant is a compile error here and every read-only
    /// `FieldContext` walker (error counting, …) routes through it instead of
    /// re-spelling the dispatch. Includes REPEATING children (array/blocks
    /// rows), which per-instance walkers may choose to skip via
    /// [`Self::non_repeating_children_mut`].
    #[must_use]
    pub fn child_field_slices(&self) -> Vec<&[FieldContext]> {
        match self {
            FieldContext::Group(f) | FieldContext::Collapsible(f) => vec![&f.sub_fields],
            FieldContext::Row(f) => vec![&f.sub_fields],
            FieldContext::Tabs(f) => f.tabs.iter().map(|t| t.sub_fields.as_slice()).collect(),
            FieldContext::Array(f) => f
                .rows
                .as_ref()
                .map(|rs| rs.iter().map(|r| r.sub_fields.as_slice()).collect())
                .unwrap_or_default(),
            FieldContext::Blocks(f) => f
                .rows
                .as_ref()
                .map(|rs| rs.iter().map(|r| r.sub_fields.as_slice()).collect())
                .unwrap_or_default(),
            FieldContext::Text(_)
            | FieldContext::Email(_)
            | FieldContext::Password(_)
            | FieldContext::Json(_)
            | FieldContext::Textarea(_)
            | FieldContext::Number(_)
            | FieldContext::Code(_)
            | FieldContext::Richtext(_)
            | FieldContext::Date(_)
            | FieldContext::Checkbox(_)
            | FieldContext::Select(_)
            | FieldContext::Radio(_)
            | FieldContext::Relationship(_)
            | FieldContext::Upload(_)
            | FieldContext::Join(_) => Vec::new(),
        }
    }

    /// Mutable NON-REPEATING composite children — the sub-field groups that
    /// share this field's single data scope (Group/Collapsible/Row share
    /// one; Tabs split by pane, same scope). Array/Blocks ROWS are
    /// repeating (each row its own per-row data scope) and leaves have
    /// none — both classify as [`NonRepeatingChildren::None`] here. Used by
    /// per-form-instance walkers (display conditions) so a nested condition
    /// evaluates against the same form data. ONE exhaustive match — a new
    /// composite is a compile error, forcing the descend/skip decision.
    pub fn non_repeating_children_mut(&mut self) -> NonRepeatingChildren<'_> {
        match self {
            FieldContext::Group(f) | FieldContext::Collapsible(f) => {
                NonRepeatingChildren::Flat(&mut f.sub_fields)
            }
            FieldContext::Row(f) => NonRepeatingChildren::Flat(&mut f.sub_fields),
            FieldContext::Tabs(f) => NonRepeatingChildren::Tabs(&mut f.tabs),
            // Repeating containers (Array/Blocks rows carry their own per-row
            // data scope) and scalar leaves have no non-repeating children to
            // descend into at this level.
            FieldContext::Array(_)
            | FieldContext::Blocks(_)
            | FieldContext::Text(_)
            | FieldContext::Email(_)
            | FieldContext::Password(_)
            | FieldContext::Json(_)
            | FieldContext::Textarea(_)
            | FieldContext::Number(_)
            | FieldContext::Code(_)
            | FieldContext::Richtext(_)
            | FieldContext::Date(_)
            | FieldContext::Checkbox(_)
            | FieldContext::Select(_)
            | FieldContext::Radio(_)
            | FieldContext::Relationship(_)
            | FieldContext::Upload(_)
            | FieldContext::Join(_) => NonRepeatingChildren::None,
        }
    }
}

/// The non-repeating composite children of a [`FieldContext`] — see
/// [`FieldContext::non_repeating_children_mut`].
pub enum NonRepeatingChildren<'a> {
    /// A leaf, or a repeating (array/blocks) composite — no non-repeating
    /// children to walk in this field's data scope.
    None,
    /// One flat sub-field list sharing the parent's scope
    /// (Group/Collapsible/Row); its defs come from `FieldDefinition::fields`.
    Flat(&'a mut Vec<FieldContext>),
    /// Per-tab sub-field lists (Tabs); tab defs come from
    /// `FieldDefinition::tabs`.
    Tabs(&'a mut Vec<super::composites::TabPanel>),
}

#[cfg(test)]
mod tests {
    use super::super::test_helpers::make_base;
    use super::*;

    /// The internally-tagged enum produces `{"field_type": "...", ...flat
    /// keys...}` with no per-variant wrapper object.
    #[test]
    fn untagged_enum_produces_no_variant_wrapper() {
        let f = TextField {
            base: make_base("title"),
            has_many: None,
            tags: None,
        };
        let v = serde_json::to_value(FieldContext::Text(f)).unwrap();
        // Internally tagged: no `{"Text": {...}}` wrapper — the keys are at root.
        assert!(v.is_object());
        assert!(v.get("Text").is_none());
        assert_eq!(v["name"], "title");
    }
}
